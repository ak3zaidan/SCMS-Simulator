//! Medium access: `mac/80211p/edca-ocb` (04-models.md §4.3) and
//! `mac/80211p/slotted-abstraction` (§4.9), with the CBR measurement of §4.5.
//!
//! # OCB is why this state machine is small
//!
//! In outside-the-context-of-a-BSS mode there is no association, no ACK for a
//! group-addressed frame, and therefore **no retransmission and no contention-window
//! doubling**: EN 302 663 Annex C.4.2's note says it outright — "in broadcast mode the
//! backoff procedure is only invoked once during the initial listening (AIFS) ... due to
//! the lack of ACKs in broadcast transmissions. Therefore CW is always CWmin and never
//! doubled." A CAM, a BSM and a DENM are all group-addressed, so for the traffic this
//! simulator exists to study the retry logic is dead code. [`EdcaOcbMac`] implements that
//! literally: a broadcast frame is granted once, and
//! `a_broadcast_frame_is_never_retransmitted` asserts that no path re-enqueues it and that
//! `CW` never leaves `CWmin`.
//!
//! # Being driven rather than driving
//!
//! The published `Mac` has the MAC schedule its own `MacTimer` events, which needs the
//! engine's payload enum (build decision D8). [`crate::traits::Mac::poll`] is the seam
//! instead: the engine calls it from the timer handler it owns, and a test calls it
//! directly with a clock it sets by hand — which is what makes the backoff freeze, the
//! AIFS wait and the internal-collision rule testable without a kernel.
//!
//! Being driven does not mean the engine decides *when*. [`crate::traits::Mac::next_poll_at`]
//! reports the instant the earliest backoff expires, so the engine schedules its timer
//! for the slot boundary the MAC computed and [`crate::types::TxGrant::at`] is that
//! instant rather than whenever the poll happened. Without it the poll cadence sets the
//! collision structure instead of the 13 µs slot.
//!
//! # A queue drains
//!
//! `enqueue` arms an access category's backoff when none is pending; a grant re-arms it
//! whenever the queue is still non-empty, one AIFS after the granted frame's air time.
//! That is what makes the queue drain. It is also the only re-arm: there is still no
//! retransmission and no contention-window doubling, because there is still no ACK.
//!
//! # The CBR window
//!
//! [`CbrMeter`] accumulates busy intervals and reports `T_busy / T_CBR` over the last
//! 100 ms [EN 302 571 §4.2.10.1]. Busy means the received signal strength exceeded
//! −85 dBm, which is the PHY's [`crate::phy::CcaConfig::busy_dbm`]; the node's own
//! transmissions count as busy too, because the medium is occupied and because every
//! simulator this is compared against counts them (recorded on the card, since the
//! standard defines the measurement on receive).

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime};

use crate::phy::air_time;
use crate::traits::Mac;
use crate::types::{
    AccessCategory, CcaState, ChannelId, DropCause, MacSdu, ResourceModel, TxGrant, TxHandle,
    timing,
};

/// The default queue depth per access category.
///
/// Nothing in the cited standards fixes a queue depth — it is an implementation choice of
/// the radio — so this is a `todo-calibrate` parameter with a plan, and the value is large
/// enough that a 10 Hz CAM generator cannot fill it inside a CBR window.
pub const DEFAULT_QUEUE_DEPTH: usize = 64;

/// The channel-busy-ratio meter of one node and channel (04-models.md §4.5).
///
/// `CBR = T_busy / T_CBR` over the last `T_CBR` = 100 ms. Intervals older than the window
/// are pruned as they fall out of it, so the meter's memory is bounded by the number of
/// busy periods in 100 ms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CbrMeter {
    window: Duration,
    /// Closed busy intervals, in start order.
    busy: VecDeque<(SimTime, SimTime)>,
    /// The instant the medium became busy, while it still is.
    open: Option<SimTime>,
}

impl CbrMeter {
    /// A meter over the standard 100 ms window.
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(timing::T_CBR)
    }

    /// A meter over a caller-chosen window.
    #[must_use]
    pub fn with_window(window: Duration) -> Self {
        Self {
            window,
            busy: VecDeque::new(),
            open: None,
        }
    }

    /// The measurement window.
    #[must_use]
    pub const fn window(&self) -> Duration {
        self.window
    }

    /// Records that the medium became busy at `t`. Idempotent while it stays busy.
    pub fn busy_from(&mut self, t: SimTime) {
        if self.open.is_none() {
            self.open = Some(t);
        }
    }

    /// Records that the medium became idle at `t`.
    pub fn idle_at(&mut self, t: SimTime) {
        if let Some(from) = self.open.take()
            && t > from
        {
            self.busy.push_back((from, t));
            self.prune(t);
        }
    }

    /// Records a complete busy interval, for a caller that knows both ends at once — an
    /// own transmission, or a frame whose arrival window the PHY has already computed.
    pub fn note_busy(&mut self, from: SimTime, to: SimTime) {
        if to > from {
            self.busy.push_back((from, to));
            // Intervals may be appended out of order (an own transmission recorded after
            // a received frame that started later); one sort of a bounded deque keeps the
            // window arithmetic simple and the answer order-independent.
            let mut v: Vec<(SimTime, SimTime)> = self.busy.iter().copied().collect();
            v.sort_unstable();
            self.busy = v.into();
            self.prune(to);
        }
    }

    fn prune(&mut self, now: SimTime) {
        let cutoff = now.saturating_sub(self.window.as_nanos());
        while let Some(&(_, to)) = self.busy.front() {
            if to <= cutoff {
                self.busy.pop_front();
            } else {
                break;
            }
        }
    }

    /// The busy time inside the window ending at `now`, nanoseconds.
    ///
    /// Overlapping intervals are merged before they are counted, so two receptions that
    /// overlap in time make the medium busy once and the ratio can never exceed one.
    #[must_use]
    pub fn busy_ns(&self, now: SimTime) -> u64 {
        let from = now.saturating_sub(self.window.as_nanos());
        let mut spans: Vec<(SimTime, SimTime)> = self
            .busy
            .iter()
            .copied()
            .chain(self.open.map(|o| (o, now)))
            .filter_map(|(a, b)| {
                let lo = a.max(from);
                let hi = b.min(now);
                (hi > lo).then_some((lo, hi))
            })
            .collect();
        spans.sort_unstable();
        let mut total = 0u64;
        let mut current: Option<(SimTime, SimTime)> = None;
        for (lo, hi) in spans {
            match current {
                None => current = Some((lo, hi)),
                Some((c_lo, c_hi)) => {
                    if lo <= c_hi {
                        current = Some((c_lo, c_hi.max(hi)));
                    } else {
                        total += c_hi - c_lo;
                        current = Some((lo, hi));
                    }
                }
            }
        }
        if let Some((c_lo, c_hi)) = current {
            total += c_hi - c_lo;
        }
        total
    }

    /// The channel busy ratio over the window ending at `now`, `0.0..=1.0`.
    ///
    /// Before the first full window has elapsed the denominator is the time that has
    /// actually passed, so a run does not start with an artificially low CBR; at `now`
    /// zero it is zero.
    #[must_use]
    pub fn cbr(&self, now: SimTime) -> f64 {
        let denominator = self.window.as_nanos().min(now);
        if denominator == 0 {
            return 0.0;
        }
        (self.busy_ns(now) as f64 / denominator as f64).clamp(0.0, 1.0)
    }
}

impl Default for CbrMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// One access category's backoff state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Backoff {
    /// Slots still to count down.
    pub remaining: u32,
    /// Slots originally drawn, for the record.
    pub drawn: u32,
    /// The instant the countdown may resume. `None` while it is frozen on a busy medium.
    pub resume_at: Option<SimTime>,
}

/// The per-node, per-channel state of the EDCA MAC.
#[derive(Debug, Clone, Default)]
struct NodeState {
    /// One queue per access category, indexed by [`AccessCategory`] order.
    queues: [VecDeque<MacSdu>; 4],
    backoff: [Option<Backoff>; 4],
    busy: bool,
    /// When the medium last became idle. `None` before the first idle report, which is
    /// treated as "idle since time zero": a run starts on a quiet channel.
    idle_since: Option<SimTime>,
    cbr: CbrMeter,
    /// Frames granted, per access category.
    granted: [u64; 4],
    /// Internal collisions: a lower-priority category that was eligible in the same slot
    /// as the winner.
    internal_collisions: u64,
    /// Air time granted at this node, nanoseconds — the MAC side of invariant I-R3.
    granted_airtime_ns: u64,
    /// Frames dropped because a queue was full.
    dropped: u64,
    /// Retransmissions. Zero for a run that only sends group-addressed frames, which is
    /// what `a_broadcast_frame_is_never_retransmitted` asserts.
    retransmissions: u64,
}

/// The instant an armed backoff's countdown reaches zero, if it is running.
///
/// `None` while the countdown is frozen on a busy medium, which is what `resume_at`
/// being `None` means.
fn due_at(b: &Backoff) -> Option<SimTime> {
    b.resume_at
        .map(|r| r + u64::from(b.remaining) * timing::SLOT_TIME.as_nanos())
}

fn ac_index(ac: AccessCategory) -> usize {
    match ac {
        AccessCategory::Bk => 0,
        AccessCategory::Be => 1,
        AccessCategory::Vi => 2,
        AccessCategory::Vo => 3,
    }
}

/// `mac/80211p/edca-ocb` — the full EDCA state machine in OCB mode (04-models.md §4.3).
#[derive(Debug)]
pub struct EdcaOcbMac {
    card: ModelCard,
    queue_depth: usize,
    nodes: BTreeMap<(u32, u16), NodeState>,
}

impl EdcaOcbMac {
    /// The model's id.
    pub const ID: &'static str = "mac/80211p/edca-ocb";

    /// The MAC with the default queue depth.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: edca_card(),
            queue_depth: DEFAULT_QUEUE_DEPTH,
            nodes: BTreeMap::new(),
        }
    }

    /// The MAC with a caller-chosen queue depth.
    #[must_use]
    pub fn with_queue_depth(mut self, depth: usize) -> Self {
        self.queue_depth = depth;
        self
    }

    fn state(&mut self, node: NodeId, ch: ChannelId) -> &mut NodeState {
        self.nodes.entry((node.index(), ch.0)).or_default()
    }

    fn state_ref(&self, node: NodeId, ch: ChannelId) -> Option<&NodeState> {
        self.nodes.get(&(node.index(), ch.0))
    }

    /// The contention window in force for an access category.
    ///
    /// Always `CWmin`, for every category, for every frame: OCB never doubles it
    /// (EN 302 663 Annex C.4.2). The method exists so that a test can assert it, and so
    /// that a reader looking for the doubling finds this comment instead of a bug.
    #[must_use]
    pub fn contention_window(&self, ac: AccessCategory) -> u32 {
        ac.cw_min()
    }

    /// How many frames this node has been granted in one access category.
    #[must_use]
    pub fn granted(&self, node: NodeId, ch: ChannelId, ac: AccessCategory) -> u64 {
        self.state_ref(node, ch)
            .map_or(0, |s| s.granted[ac_index(ac)])
    }

    /// How many retransmissions this node has made. Zero unless individually addressed
    /// frames are used.
    #[must_use]
    pub fn retransmissions(&self, node: NodeId, ch: ChannelId) -> u64 {
        self.state_ref(node, ch).map_or(0, |s| s.retransmissions)
    }

    /// How many internal collisions this node has resolved.
    #[must_use]
    pub fn internal_collisions(&self, node: NodeId, ch: ChannelId) -> u64 {
        self.state_ref(node, ch)
            .map_or(0, |s| s.internal_collisions)
    }

    /// The air time this node has been granted, nanoseconds (invariant I-R3).
    #[must_use]
    pub fn granted_airtime_ns(&self, node: NodeId, ch: ChannelId) -> u64 {
        self.state_ref(node, ch).map_or(0, |s| s.granted_airtime_ns)
    }

    /// How many frames this node dropped for a full queue.
    #[must_use]
    pub fn dropped(&self, node: NodeId, ch: ChannelId) -> u64 {
        self.state_ref(node, ch).map_or(0, |s| s.dropped)
    }

    /// The backoff state of one access category, for inspection and for tests.
    #[must_use]
    pub fn backoff(&self, node: NodeId, ch: ChannelId, ac: AccessCategory) -> Option<Backoff> {
        self.state_ref(node, ch)
            .and_then(|s| s.backoff[ac_index(ac)])
    }

    /// How many frames are queued in one access category.
    #[must_use]
    pub fn queue_len(&self, node: NodeId, ch: ChannelId, ac: AccessCategory) -> usize {
        self.state_ref(node, ch)
            .map_or(0, |s| s.queues[ac_index(ac)].len())
    }

    /// Records a busy interval on a node's channel, for the CBR measurement.
    ///
    /// The engine calls this for every frame whose received power exceeded the CBR
    /// threshold; [`Mac::on_cca`] covers the same ground for a MAC driven by CCA
    /// transitions instead.
    pub fn note_busy(&mut self, node: NodeId, ch: ChannelId, from: SimTime, to: SimTime) {
        self.state(node, ch).cbr.note_busy(from, to);
    }
}

impl Default for EdcaOcbMac {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for EdcaOcbMac {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Mac<C> for EdcaOcbMac {
    fn tier(&self) -> Tier {
        Tier::High
    }

    fn enqueue(
        &mut self,
        ctx: &mut C,
        node: NodeId,
        sdu: MacSdu,
        ac: AccessCategory,
    ) -> core::result::Result<(), DropCause> {
        if sdu.frame.bytes > timing::MAX_MSDU_BYTES {
            return Err(DropCause::TooLarge {
                bytes: sdu.frame.bytes,
                cap: timing::MAX_MSDU_BYTES,
            });
        }
        let now = ctx.now();
        let depth = self.queue_depth;
        // The backoff draw needs the RNG before the node state is borrowed mutably, and
        // it must happen unconditionally so that the draw count does not depend on the
        // medium state: a model whose stream position depended on the channel would
        // replay differently after any timing change. The draw is used only when the
        // medium is not already idle for AIFS.
        let slots = ctx
            .rng(RngDomain::MacBackoff, EntityRef::Node(node))
            .below(u64::from(ac.cw_min()) + 1) as u32;
        let state = self.state(node, ch_of(&sdu));
        let index = ac_index(ac);
        if state.queues[index].len() >= depth {
            state.dropped += 1;
            return Err(DropCause::QueueFull {
                ac: ac.label(),
                depth,
            });
        }
        state.queues[index].push_back(sdu);
        if state.backoff[index].is_none() {
            let idle_for = match (state.busy, state.idle_since) {
                (true, _) => None,
                (false, Some(since)) => Some(Duration::between(since, now)),
                // No CCA report yet: the channel has been idle since time zero.
                (false, None) => Some(Duration::from_nanos(now)),
            };
            let aifs_met = idle_for.is_some_and(|d| d.as_nanos() >= ac.aifs().as_nanos());
            state.backoff[index] = Some(if aifs_met {
                // "On enqueue with idle medium for AIFS, transmit immediately."
                Backoff {
                    remaining: 0,
                    drawn: 0,
                    resume_at: Some(now),
                }
            } else {
                Backoff {
                    remaining: slots,
                    drawn: slots,
                    resume_at: if state.busy {
                        None
                    } else {
                        Some(
                            state
                                .idle_since
                                .unwrap_or(0)
                                .max(now.saturating_sub(ac.aifs().as_nanos()))
                                + ac.aifs().as_nanos(),
                        )
                    },
                }
            });
        }
        Ok(())
    }

    fn on_cca(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId, cca: CcaState) {
        let now = ctx.now();
        let state = self.state(node, ch);
        match cca {
            CcaState::Busy { .. } => {
                if !state.busy {
                    state.busy = true;
                    state.idle_since = None;
                    state.cbr.busy_from(now);
                    // Freeze every countdown: the medium is occupied.
                    for b in state.backoff.iter_mut().flatten() {
                        b.resume_at = None;
                    }
                }
            }
            CcaState::Idle => {
                if state.busy {
                    state.busy = false;
                    state.idle_since = Some(now);
                    state.cbr.idle_at(now);
                    // Resume each countdown one AIFS after the medium went idle.
                    for (i, slot) in state.backoff.iter_mut().enumerate() {
                        if let Some(b) = slot {
                            b.resume_at = Some(now + aifs_of_index(i).as_nanos());
                        }
                    }
                } else if state.idle_since.is_none() {
                    state.idle_since = Some(now);
                }
            }
        }
    }

    fn on_tx_done(&mut self, _ctx: &mut C, node: NodeId, h: TxHandle) {
        let state = self.state(node, h.channel);
        // A node's own transmission occupied the medium: it belongs in the CBR window.
        state.cbr.note_busy(h.start, h.end);
        // And that is the whole of it. There is no ACK to wait for, so nothing is
        // re-queued, no retry counter moves, and CW stays at CWmin
        // (EN 302 663 Annex C.4.2).
    }

    fn cbr(&self, node: NodeId, ch: ChannelId, now: SimTime) -> f64 {
        // The window ends at `now`, not at whenever this meter last heard anything: a
        // channel that has fallen quiet decays to zero, which is what the DCC loop on the
        // other end of `Dcc::on_cbr` is entitled to be told.
        self.state_ref(node, ch).map_or(0.0, |s| s.cbr.cbr(now))
    }

    fn resource_model(&self) -> ResourceModel {
        ResourceModel::Csma { edca: true }
    }

    fn next_poll_at(&self, node: NodeId, ch: ChannelId) -> Option<SimTime> {
        let state = self.state_ref(node, ch)?;
        if state.busy {
            // Every countdown is frozen; the next thing that happens is a CCA report.
            return None;
        }
        (0..4)
            .filter(|&i| !state.queues[i].is_empty())
            .filter_map(|i| state.backoff[i].and_then(|b| due_at(&b)))
            .min()
    }

    fn poll(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId) -> Option<TxGrant> {
        let now = ctx.now();
        let state = self.nodes.get_mut(&(node.index(), ch.0))?;
        if state.busy {
            return None;
        }
        // Highest access category first: an internal collision resolves to the higher AC
        // (04-models.md §4.3).
        let mut winner: Option<(usize, SimTime)> = None;
        let mut eligible = 0u32;
        for index in (0..4).rev() {
            if state.queues[index].is_empty() {
                continue;
            }
            let Some(mut b) = state.backoff[index] else {
                continue;
            };
            let Some(resume) = b.resume_at else {
                continue;
            };
            if now < resume {
                continue;
            }
            // The instant this AC's countdown reaches zero. `now >= due` is exactly the
            // old `ticks >= remaining` test, written so the instant itself is available
            // for the grant.
            let due = resume + u64::from(b.remaining) * timing::SLOT_TIME.as_nanos();
            let ticks = (now - resume) / timing::SLOT_TIME.as_nanos();
            if now >= due {
                b.remaining = 0;
                state.backoff[index] = Some(b);
                eligible += 1;
                if winner.is_none() {
                    winner = Some((index, due));
                }
            } else {
                b.remaining -= ticks as u32;
                b.resume_at = Some(resume + ticks * timing::SLOT_TIME.as_nanos());
                state.backoff[index] = Some(b);
            }
        }
        let (index, due) = winner?;
        if eligible > 1 {
            state.internal_collisions += u64::from(eligible - 1);
        }
        let sdu = state.queues[index].pop_front()?;
        let drawn = state.backoff[index].map_or(0, |b| b.drawn);
        state.backoff[index] = None;
        state.granted[index] += 1;
        let air = air_time(sdu.frame.bytes, sdu.frame.mcs);
        state.granted_airtime_ns += air.as_nanos();
        // A frame still queued in this AC starts its own backoff procedure now. Without
        // this the queue never re-armed — `enqueue` arms only when no backoff is pending
        // and `poll` cleared it here — so every frame queued behind another in the same
        // access category was stranded for ever, and the MAC delivered one frame per
        // enqueue instead of draining. That bites under exactly the congestion this
        // simulator exists to study: a busy medium or a DCC delay makes frames
        // accumulate, and from then on the MAC runs one frame behind.
        let more_queued = !state.queues[index].is_empty();
        let grant = TxGrant {
            sdu,
            ac: ac_of_index(index),
            // The slot boundary the backoff computed, not the instant the engine happened
            // to poll at. An engine that schedules its MacTimer from `next_poll_at` sees
            // `at == now`; a coarse poll sees an instant in the past, which is the honest
            // report that the frame is overdue.
            at: due,
            backoff_slots: drawn,
            // One attempt, always: OCB has no ACK, so there is never a second one for a
            // group-addressed frame.
            attempt: 1,
        };
        if more_queued {
            let ac = grant.ac;
            // Drawn here rather than at the top of `poll` on purpose: a grant is a causal
            // event, so the stream position follows the sequence of grants, while a draw
            // per poll would make it follow the engine's poll cadence.
            let slots = ctx
                .rng(RngDomain::MacBackoff, EntityRef::Node(node))
                .below(u64::from(ac.cw_min()) + 1) as u32;
            let state = self.state(node, ch);
            state.backoff[index] = Some(Backoff {
                remaining: slots,
                drawn: slots,
                // The medium is occupied by the frame just granted; the next contention
                // starts one AIFS after it ends. A `on_cca(Busy)` report for our own
                // transmission freezes this again and the matching `Idle` re-computes the
                // same instant, so a MAC driven by CCA and one driven by `poll` alone
                // agree (EN 302 663 Annex C.4.2: the procedure runs once per frame).
                resume_at: Some(air.after(due) + ac.aifs().as_nanos()),
            });
        }
        Some(grant)
    }
}

impl CbrMeter {
    /// The last instant this meter has information about: the end of the last closed
    /// busy interval or the start of the open one, whichever is later, and zero for a
    /// meter that has seen nothing.
    ///
    /// **Not a clock.** It is an inspection accessor — "when did this node last hear
    /// anything" — and reading [`CbrMeter::cbr`] at it reports the load of whenever the
    /// channel was last busy rather than the load now: a node busy from 0 to 50 ms and
    /// silent afterwards reads 1.0 for ever, which pins every [`crate::traits::Dcc`]
    /// model at `δ_min`. [`Mac::cbr`] takes the instant to measure at for exactly this
    /// reason.
    #[must_use]
    pub fn last_known(&self) -> SimTime {
        let closed = self.busy.back().map_or(0, |&(_, to)| to);
        closed.max(self.open.unwrap_or(0))
    }
}

/// The access category at an internal index.
fn ac_of_index(index: usize) -> AccessCategory {
    match index {
        0 => AccessCategory::Bk,
        1 => AccessCategory::Be,
        2 => AccessCategory::Vi,
        _ => AccessCategory::Vo,
    }
}

fn aifs_of_index(index: usize) -> Duration {
    ac_of_index(index).aifs()
}

fn ch_of(sdu: &MacSdu) -> ChannelId {
    sdu.frame.channel
}

fn en302663_mac() -> Source {
    Source {
        kind: SourceKind::Standard,
        reference: "ETSI EN 302 663 V1.3.1 Annex C.4.2, C.4.4 and C.5, Tables C.3-C.6 \
                    (R1 §B.1-B.2), via 04-models.md §4.3"
            .to_string(),
        accessed: None,
        note: Some(
            "The AC parameters are attributed to IEEE 802.11-2016 Table 9-138 (not 9-137) \
             and the PHY constants to Table 17-21."
                .to_string(),
        ),
    }
}

fn edca_card() -> ModelCard {
    let mut card = ModelCard::new(
        EdcaOcbMac::ID,
        Family::Mac,
        "1.0.0",
        "EDCA in OCB mode: four access categories with their own AIFS and contention \
         window, backoff frozen on a busy medium, internal collisions resolved to the \
         higher category, no ACK and therefore no retransmission, and the 100 ms CBR \
         measurement.",
    );
    card.tier = vec![Tier::High];
    card.equations = vec![
        Equation::new("AIFS", "AIFS[AC] = AIFSN[AC] × aSlotTime + aSIFSTime"),
        Equation {
            name: "backoff".to_string(),
            latex_or_text: "uniform in [0, CWmin] slots, counted down while the medium is \
                            idle, frozen while busy, resumed one AIFS after it goes idle"
                .to_string(),
            notes: Some(
                "CW never doubles: OCB has no group-addressed ACK, so the procedure runs \
                 once (EN 302 663 Annex C.4.2 note)."
                    .to_string(),
            ),
        },
        Equation::new("CBR", "CBR = T_busy / T_CBR, T_CBR = 100 ms"),
    ];
    card.parameters = vec![
        Parameter {
            name: "slot_time_us".to_string(),
            unit: "µs".to_string(),
            default: serde_json::json!(13),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "sifs_us".to_string(),
            unit: "µs".to_string(),
            default: serde_json::json!(32),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "cw_min_vo".to_string(),
            unit: "slots".to_string(),
            default: serde_json::json!(3),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "cw_min_vi".to_string(),
            unit: "slots".to_string(),
            default: serde_json::json!(7),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "cw_min_be_bk".to_string(),
            unit: "slots".to_string(),
            default: serde_json::json!(15),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "aifsn".to_string(),
            unit: "slots".to_string(),
            default: serde_json::json!([9, 6, 3, 2]),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "t_cbr_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(100),
            range: None,
            source: Source::new(
                SourceKind::Standard,
                "ETSI EN 302 571 V2.1.1 §4.2.10.1 and TS 102 687 Table 3, via \
                 04-models.md §4.4",
            ),
            calibration: None,
        },
        Parameter {
            name: "queue_depth".to_string(),
            unit: "frames".to_string(),
            default: serde_json::json!(DEFAULT_QUEUE_DEPTH),
            range: Some(vec![serde_json::json!(1), serde_json::json!(4_096)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "no cited standard fixes a per-AC queue depth; it is a radio \
                            implementation choice"
                    .to_string(),
                accessed: None,
                note: Some(
                    "64 frames is more than a 10 Hz generator can fill inside a CBR \
                     window, so the default cannot silently shape a scalability result."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Read the queue depth from the target OBU's datasheet (R7) or measure the \
                 depth at which a saturated MK5 starts dropping."
                    .to_string(),
            ),
        },
        Parameter {
            name: "use_acks".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(false),
            range: Some(vec![serde_json::json!(false)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "EN 302 663 Annex C.4.2: no group-addressed ACKs; Veins \
                            Mac1609_4 useAcks = false (R1 §B.4)"
                    .to_string(),
                accessed: None,
                note: Some(
                    "The range admits only false: this model does not implement the retry \
                     path, because the traffic it exists for is all group-addressed."
                        .to_string(),
                ),
            },
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "Every frame is group-addressed, so no ACK is expected, no frame is \
         retransmitted, and CW is CWmin for every attempt."
            .to_string(),
        "The engine reports CCA transitions; between two reports the medium state is \
         unchanged."
            .to_string(),
        "A node's own transmissions count as busy in its CBR window, which is what the \
         simulators this is compared against do, although the standard defines the \
         measurement on receive."
            .to_string(),
        "One backoff draw per enqueue, whether or not the medium turns out to be idle for \
         AIFS: a draw count that depended on the channel state would make the stream \
         position depend on timing."
            .to_string(),
        "One further backoff draw per grant that leaves the queue non-empty: the next \
         frame in the access category starts its own backoff procedure one AIFS after \
         the granted frame's air time, so a queue drains one frame per contention round \
         rather than one frame per enqueue. A grant is a causal event, so the draw count \
         follows the sequence of grants and not the engine's poll cadence."
            .to_string(),
        "The channel busy ratio is measured over the window ending at the instant the \
         caller asks for (Mac::cbr takes it), so a channel that falls silent decays to \
         zero rather than holding its last busy value."
            .to_string(),
    ];
    card.limitations = vec![
        "No RTS/CTS (the Veins reference sets dot11RTSThreshold to 12,000 bit, which \
         disables it for any frame this simulator sends) and no retry limits."
            .to_string(),
        "No IEEE 1609.4 channel switching: its constants are UNVERIFIED at the primary \
         level (04-models.md §4.1) and the default is continuous CCH operation."
            .to_string(),
        "The queue is FIFO per access category with no ageing or preemption.".to_string(),
    ];
    card.ignores = vec![
        "Nothing relative to the slotted abstraction below it; relative to reality: \
         chipset-specific deferral quirks and the 1609.4 guard interval."
            .to_string(),
    ];
    card.sources = vec![
        en302663_mac(),
        Source::new(
            SourceKind::Code,
            "Veins Mac1609_4 reference configuration (R1 §B.4): useAcks false, \
             dot11RTSThreshold 12,000 bit, ackLength 112 bit",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![en302663_mac()],
        tests: vec![
            "an_idle_medium_grants_immediately_after_aifs".to_string(),
            "a_busy_medium_freezes_the_backoff".to_string(),
            "a_broadcast_frame_is_never_retransmitted".to_string(),
            "an_internal_collision_resolves_to_the_higher_category".to_string(),
            "cbr_of_an_idle_channel_is_zero_and_of_a_saturated_one_approaches_one".to_string(),
            "mac_airtime_accounting_sums_to_the_transmitted_time".to_string(),
            "a_saturated_queue_drains_every_frame".to_string(),
            "a_queue_keeps_draining_across_a_busy_medium".to_string(),
            "a_channel_that_falls_silent_decays_to_zero".to_string(),
            "a_grant_is_at_the_slot_the_backoff_computed_not_at_the_poll".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["mac-backoff".to_string()],
    };
    card
}

// =========================================================================================
// `mac/80211p/slotted-abstraction`
// =========================================================================================

/// `mac/80211p/slotted-abstraction` — the medium tier's MAC (04-models.md §4.9).
///
/// Contention is resolved per 13 µs slot: a frame draws a slot uniformly in
/// `[0, CWmin]`, transmits at that slot boundary, and two nodes in CCA range that drew
/// the same slot collide — which the PHY sees as two overlapping arrivals and reports
/// through its own error model. There is no per-AC AIFS, no backoff freeze and no
/// internal-collision rule, which is exactly the list the tier table says this tier
/// ignores.
#[derive(Debug)]
pub struct SlottedMac {
    card: ModelCard,
    queue_depth: usize,
    cw: u32,
    nodes: BTreeMap<(u32, u16), SlottedState>,
}

#[derive(Debug, Clone, Default)]
struct SlottedState {
    queue: VecDeque<(MacSdu, AccessCategory, SimTime, u32)>,
    cbr: CbrMeter,
    granted: u64,
    granted_airtime_ns: u64,
    dropped: u64,
    busy: bool,
}

impl SlottedMac {
    /// The model's id.
    pub const ID: &'static str = "mac/80211p/slotted-abstraction";

    /// The MAC with the AC_VO contention window, which is the one a CAM or BSM uses.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: slotted_card(),
            queue_depth: DEFAULT_QUEUE_DEPTH,
            cw: AccessCategory::Vo.cw_min(),
            nodes: BTreeMap::new(),
        }
    }

    /// The slot a queued frame is waiting for, if any.
    #[must_use]
    pub fn pending_slot(&self, node: NodeId, ch: ChannelId) -> Option<u32> {
        self.nodes
            .get(&(node.index(), ch.0))
            .and_then(|s| s.queue.front().map(|(_, _, _, slot)| *slot))
    }

    /// The air time granted at this node, nanoseconds.
    #[must_use]
    pub fn granted_airtime_ns(&self, node: NodeId, ch: ChannelId) -> u64 {
        self.nodes
            .get(&(node.index(), ch.0))
            .map_or(0, |s| s.granted_airtime_ns)
    }

    /// Records a busy interval, for the CBR measurement.
    pub fn note_busy(&mut self, node: NodeId, ch: ChannelId, from: SimTime, to: SimTime) {
        self.nodes
            .entry((node.index(), ch.0))
            .or_default()
            .cbr
            .note_busy(from, to);
    }
}

impl Default for SlottedMac {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for SlottedMac {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Mac<C> for SlottedMac {
    fn tier(&self) -> Tier {
        Tier::Medium
    }

    fn enqueue(
        &mut self,
        ctx: &mut C,
        node: NodeId,
        sdu: MacSdu,
        ac: AccessCategory,
    ) -> core::result::Result<(), DropCause> {
        if sdu.frame.bytes > timing::MAX_MSDU_BYTES {
            return Err(DropCause::TooLarge {
                bytes: sdu.frame.bytes,
                cap: timing::MAX_MSDU_BYTES,
            });
        }
        let now = ctx.now();
        let slot = ctx
            .rng(RngDomain::MacBackoff, EntityRef::Node(node))
            .below(u64::from(self.cw) + 1) as u32;
        let depth = self.queue_depth;
        let state = self
            .nodes
            .entry((node.index(), sdu.frame.channel.0))
            .or_default();
        if state.queue.len() >= depth {
            state.dropped += 1;
            return Err(DropCause::QueueFull {
                ac: ac.label(),
                depth,
            });
        }
        state.queue.push_back((sdu, ac, now, slot));
        Ok(())
    }

    fn on_cca(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId, cca: CcaState) {
        let now = ctx.now();
        let state = self.nodes.entry((node.index(), ch.0)).or_default();
        match cca {
            CcaState::Busy { .. } => {
                if !state.busy {
                    state.busy = true;
                    state.cbr.busy_from(now);
                }
            }
            CcaState::Idle => {
                if state.busy {
                    state.busy = false;
                    state.cbr.idle_at(now);
                }
            }
        }
    }

    fn on_tx_done(&mut self, _ctx: &mut C, node: NodeId, h: TxHandle) {
        self.nodes
            .entry((node.index(), h.channel.0))
            .or_default()
            .cbr
            .note_busy(h.start, h.end);
    }

    fn cbr(&self, node: NodeId, ch: ChannelId, now: SimTime) -> f64 {
        self.nodes
            .get(&(node.index(), ch.0))
            .map_or(0.0, |s| s.cbr.cbr(now))
    }

    fn resource_model(&self) -> ResourceModel {
        ResourceModel::Csma { edca: false }
    }

    fn next_poll_at(&self, node: NodeId, ch: ChannelId) -> Option<SimTime> {
        let state = self.nodes.get(&(node.index(), ch.0))?;
        if state.busy {
            return None;
        }
        let (_, _, enqueued_at, slot) = *state.queue.front()?;
        Some(enqueued_at + u64::from(slot) * timing::SLOT_TIME.as_nanos())
    }

    fn poll(&mut self, ctx: &mut C, node: NodeId, ch: ChannelId) -> Option<TxGrant> {
        let now = ctx.now();
        let state = self.nodes.get_mut(&(node.index(), ch.0))?;
        if state.busy {
            return None;
        }
        let (_, _, enqueued_at, slot) = *state.queue.front()?;
        let due = enqueued_at + u64::from(slot) * timing::SLOT_TIME.as_nanos();
        if now < due {
            return None;
        }
        let (sdu, ac, _, slot) = state.queue.pop_front()?;
        state.granted += 1;
        state.granted_airtime_ns += air_time(sdu.frame.bytes, sdu.frame.mcs).as_nanos();
        Some(TxGrant {
            sdu,
            ac,
            // The drawn slot, not the poll instant: two nodes that drew different slots
            // must be granted at different instants or the tier has no collision
            // mechanism at all.
            at: due,
            backoff_slots: slot,
            attempt: 1,
        })
    }
}

fn slotted_card() -> ModelCard {
    let mut card = ModelCard::new(
        SlottedMac::ID,
        Family::Mac,
        "1.0.0",
        "The medium tier's MAC: slotted CSMA in 13 µs slots with one uniform draw in \
         [0, CWmin] per frame, and the same 100 ms CBR measurement as the high tier.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![Equation {
        name: "slot draw".to_string(),
        latex_or_text: "slot ~ U[0, CWmin]; transmit at enqueue + slot × aSlotTime".to_string(),
        notes: Some(
            "Two nodes in CCA range that draw the same slot overlap on the air, and the \
             PHY's error model decides what that costs."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        Parameter {
            name: "slot_time_us".to_string(),
            unit: "µs".to_string(),
            default: serde_json::json!(13),
            range: None,
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "cw".to_string(),
            unit: "slots".to_string(),
            default: serde_json::json!(AccessCategory::Vo.cw_min()),
            range: Some(vec![serde_json::json!(1), serde_json::json!(1_023)]),
            source: en302663_mac(),
            calibration: None,
        },
        Parameter {
            name: "queue_depth".to_string(),
            unit: "frames".to_string(),
            default: serde_json::json!(DEFAULT_QUEUE_DEPTH),
            range: Some(vec![serde_json::json!(1), serde_json::json!(4_096)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "no cited standard fixes a queue depth".to_string(),
                accessed: None,
                note: None,
            },
            calibration: Some("As for mac/80211p/edca-ocb.".to_string()),
        },
    ];
    card.assumptions = vec![
        "One contention window for all traffic: the AC_VO window, which is what safety \
         messages use."
            .to_string(),
    ];
    card.limitations = vec![
        "No per-AC AIFS, no backoff freeze, no internal-collision rule, no capture \
         timing: the frame either goes out in its slot or waits for the medium."
            .to_string(),
    ];
    card.ignores = vec![
        "Relative to mac/80211p/edca-ocb: per-AC AIFS, backoff freezing, internal \
         collisions and 1609.4 switching (04-models.md §4.9 tier table)."
            .to_string(),
    ];
    card.sources = vec![en302663_mac()];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "the_slotted_mac_grants_at_its_slot".to_string(),
            "mac_airtime_accounting_sums_to_the_transmitted_time".to_string(),
            "a_saturated_queue_drains_every_frame".to_string(),
            "a_queue_keeps_draining_across_a_busy_medium".to_string(),
            "a_channel_that_falls_silent_decays_to_zero".to_string(),
            "a_grant_is_at_the_slot_the_backoff_computed_not_at_the_poll".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["mac-backoff".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;
    use crate::types::{FrameDescriptor, Mcs, SduRef};
    use v2xw_core::ids::{FrameSeq, SduId};

    const CH: ChannelId = ChannelId::CCH;

    fn sdu(bytes: u32, at: SimTime) -> MacSdu {
        MacSdu {
            frame: FrameDescriptor::broadcast(
                bytes,
                Mcs::R6Qpsk12,
                SduRef::new(SduId::new(1), FrameSeq::new(1)),
            ),
            enqueued_at: at,
        }
    }

    #[test]
    fn an_idle_medium_grants_immediately_after_aifs() {
        let mut ctx = TestCtx::new(1);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        // The channel has been idle since time zero, and AC_VO's AIFS is 58 µs.
        ctx.set_now(1_000_000);
        let now = ctx.now();
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, now), AccessCategory::Vo).expect("queued");
        let grant = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert_eq!(grant.backoff_slots, 0, "an idle medium needs no backoff");
        assert_eq!(grant.ac, AccessCategory::Vo);
        assert_eq!(grant.attempt, 1);
        assert_eq!(mac.queue_len(node, CH, AccessCategory::Vo), 0);
        // And nothing more is granted.
        assert!(Mac::poll(&mut mac, &mut ctx, node, CH).is_none());
    }

    #[test]
    fn a_frame_enqueued_on_a_busy_medium_waits_for_aifs_and_its_backoff() {
        let mut ctx = TestCtx::new(2);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        // The medium goes busy, then a frame arrives.
        Mac::on_cca(
            &mut mac,
            &mut ctx,
            node,
            CH,
            CcaState::Busy { energy_dbm: -70.0 },
        );
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Vo).expect("queued");
        let b = mac.backoff(node, CH, AccessCategory::Vo).expect("drawn");
        assert!(b.resume_at.is_none(), "frozen on a busy medium");
        assert!(b.drawn <= AccessCategory::Vo.cw_min());
        // Nothing is granted while it is busy, however long we wait.
        ctx.set_now(5_000_000);
        assert!(Mac::poll(&mut mac, &mut ctx, node, CH).is_none());
        // The medium goes idle: the countdown resumes one AIFS later.
        Mac::on_cca(&mut mac, &mut ctx, node, CH, CcaState::Idle);
        let resume = mac
            .backoff(node, CH, AccessCategory::Vo)
            .and_then(|b| b.resume_at)
            .expect("resumed");
        assert_eq!(resume, 5_000_000 + AccessCategory::Vo.aifs().as_nanos());
        // One nanosecond before the last slot elapses, nothing.
        let due = resume + u64::from(b.drawn) * timing::SLOT_TIME.as_nanos();
        if b.drawn > 0 {
            ctx.set_now(due - 1);
            assert!(
                Mac::poll(&mut mac, &mut ctx, node, CH).is_none(),
                "too early"
            );
        }
        ctx.set_now(due);
        let grant = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert_eq!(grant.backoff_slots, b.drawn);
    }

    #[test]
    fn a_busy_medium_freezes_the_backoff() {
        let mut ctx = TestCtx::new(3);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        // Start busy so that a real backoff is drawn, then go idle to start counting.
        Mac::on_cca(
            &mut mac,
            &mut ctx,
            node,
            CH,
            CcaState::Busy { energy_dbm: -70.0 },
        );
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Be).expect("queued");
        let drawn = mac
            .backoff(node, CH, AccessCategory::Be)
            .expect("drawn")
            .drawn;
        ctx.set_now(1_000_000);
        Mac::on_cca(&mut mac, &mut ctx, node, CH, CcaState::Idle);
        // Count down one slot past AIFS, then freeze.
        let aifs = AccessCategory::Be.aifs().as_nanos();
        ctx.set_now(1_000_000 + aifs + timing::SLOT_TIME.as_nanos());
        let _ = Mac::poll(&mut mac, &mut ctx, node, CH);
        let after_one_slot = mac
            .backoff(node, CH, AccessCategory::Be)
            .map(|b| b.remaining);
        if drawn > 1 {
            assert_eq!(after_one_slot, Some(drawn - 1), "one slot counted");
        }
        Mac::on_cca(
            &mut mac,
            &mut ctx,
            node,
            CH,
            CcaState::Busy { energy_dbm: -70.0 },
        );
        // Ten milliseconds of busy medium must not consume a single slot.
        let later = ctx.now() + 10_000_000;
        ctx.set_now(later);
        assert!(Mac::poll(&mut mac, &mut ctx, node, CH).is_none());
        assert_eq!(
            mac.backoff(node, CH, AccessCategory::Be)
                .map(|b| b.remaining),
            after_one_slot,
            "the countdown was frozen, not merely paused in appearance"
        );
    }

    #[test]
    fn a_broadcast_frame_is_never_retransmitted() {
        let mut ctx = TestCtx::new(4);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        ctx.set_now(1_000_000);
        let now = ctx.now();
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, now), AccessCategory::Vo).expect("queued");
        let grant = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert!(grant.sdu.frame.kind.is_group_addressed());
        assert_eq!(grant.attempt, 1);
        // The transmission completes. In OCB there is no ACK to wait for.
        let handle = TxHandle {
            id: 1,
            tx: node,
            channel: CH,
            start: grant.at,
            end: grant.at + air_time(300, Mcs::R6Qpsk12).as_nanos(),
            air_time: air_time(300, Mcs::R6Qpsk12),
        };
        Mac::on_tx_done(&mut mac, &mut ctx, node, handle);
        // Nothing was re-queued, nothing was retried, and CW never moved.
        assert_eq!(mac.queue_len(node, CH, AccessCategory::Vo), 0);
        assert_eq!(mac.retransmissions(node, CH), 0);
        assert!(Mac::poll(&mut mac, &mut ctx, node, CH).is_none());
        for ac in AccessCategory::ALL {
            assert_eq!(mac.contention_window(ac), ac.cw_min());
            assert!(mac.contention_window(ac) <= ac.cw_max());
        }
        // Even after many frames, the window is still CWmin.
        for i in 0..20u64 {
            ctx.set_now(2_000_000 + i * 1_000_000);
            let now = ctx.now();
            Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, now), AccessCategory::Vo)
                .expect("queued");
            let g = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
            assert_eq!(g.attempt, 1);
        }
        assert_eq!(mac.retransmissions(node, CH), 0);
        assert_eq!(mac.contention_window(AccessCategory::Vo), 3);
    }

    /// A saturated queue drains: every frame enqueued in one access category is
    /// eventually granted, once each and in order.
    ///
    /// `enqueue` arms the backoff only when none is pending and `poll` used to clear it
    /// after a grant with no re-arm, so every frame queued behind another in the same AC
    /// was stranded for ever: three AC_VO frames enqueued together on an idle medium and
    /// polled every millisecond for 200 ms produced one grant and left two frames in the
    /// queue with `backoff = None`. Under a 10 Hz generator that never queues two frames
    /// at once the MAC looked fine, which is why the crate's 127 tests all passed.
    #[test]
    fn a_saturated_queue_drains_every_frame() {
        let mut ctx = TestCtx::new(21);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        const N: usize = 16;

        // N frames into one AC, all at once, on an idle medium.
        ctx.set_now(1_000_000);
        let at = ctx.now();
        for i in 0..N {
            Mac::enqueue(
                &mut mac,
                &mut ctx,
                node,
                sdu(300 + i as u32, at),
                AccessCategory::Vo,
            )
            .expect("queued");
        }
        assert_eq!(mac.queue_len(node, CH, AccessCategory::Vo), N);

        // Poll once a millisecond for 200 ms, which is far beyond AIFS (58 µs) plus at
        // most CWmin = 3 slots of 13 µs for every one of them.
        let mut granted = Vec::new();
        for ms in 1..=200u64 {
            ctx.set_now(ms * 1_000_000);
            while let Some(g) = Mac::poll(&mut mac, &mut ctx, node, CH) {
                granted.push(g);
            }
        }
        assert_eq!(
            granted.len(),
            N,
            "{} of {N} frames granted, {} still queued",
            granted.len(),
            mac.queue_len(node, CH, AccessCategory::Vo)
        );
        assert_eq!(mac.queue_len(node, CH, AccessCategory::Vo), 0);
        assert_eq!(mac.granted(node, CH, AccessCategory::Vo), N as u64);
        // In order, and each frame exactly once: the byte counts were made distinct so
        // that a re-granted or dropped frame shows up here.
        let sizes: Vec<u32> = granted.iter().map(|g| g.sdu.frame.bytes).collect();
        let expected: Vec<u32> = (0..N as u32).map(|i| 300 + i).collect();
        assert_eq!(sizes, expected);
        // Each grant carries its own fresh backoff draw, in [0, CWmin].
        assert!(
            granted
                .iter()
                .all(|g| g.backoff_slots <= AccessCategory::Vo.cw_min())
        );
        // The MAC's air-time ledger counted each frame once (invariant I-R3).
        let expected_air: u64 = expected
            .iter()
            .map(|b| air_time(*b, Mcs::R6Qpsk12).as_nanos())
            .sum();
        assert_eq!(mac.granted_airtime_ns(node, CH), expected_air);
        // Nothing was dropped and the queue is genuinely idle again.
        assert_eq!(mac.dropped(node, CH), 0);
        assert_eq!(mac.backoff(node, CH, AccessCategory::Vo), None);
    }

    /// The same, with the medium going busy and idle underneath: a queue that is draining
    /// keeps draining across a freeze.
    #[test]
    fn a_queue_keeps_draining_across_a_busy_medium() {
        let mut ctx = TestCtx::new(22);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        ctx.set_now(1_000_000);
        let at = ctx.now();
        for i in 0..6u32 {
            Mac::enqueue(&mut mac, &mut ctx, node, sdu(300 + i, at), AccessCategory::Be)
                .expect("queued");
        }
        let mut granted = 0;
        for ms in 1..=400u64 {
            ctx.set_now(ms * 1_000_000);
            // Ten milliseconds of busy medium every fifty.
            if ms % 50 == 0 {
                Mac::on_cca(&mut mac, &mut ctx, node, CH, CcaState::Busy { energy_dbm: -60.0 });
            } else if ms % 50 == 10 {
                Mac::on_cca(&mut mac, &mut ctx, node, CH, CcaState::Idle);
            }
            while Mac::poll(&mut mac, &mut ctx, node, CH).is_some() {
                granted += 1;
            }
        }
        assert_eq!(granted, 6);
        assert_eq!(mac.queue_len(node, CH, AccessCategory::Be), 0);
    }

    #[test]
    fn an_internal_collision_resolves_to_the_higher_category() {
        let mut ctx = TestCtx::new(5);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        // Well past the longest AIFS on an idle medium, so both categories are eligible
        // in the same instant.
        ctx.set_now(1_000_000);
        let now = ctx.now();
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, now), AccessCategory::Be).expect("queued");
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, now), AccessCategory::Vo).expect("queued");
        let first = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert_eq!(first.ac, AccessCategory::Vo, "the higher category wins");
        assert_eq!(mac.internal_collisions(node, CH), 1);
        // The loser is not dropped: it goes next.
        let second = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert_eq!(second.ac, AccessCategory::Be);
    }

    #[test]
    fn cbr_of_an_idle_channel_is_zero_and_of_a_saturated_one_approaches_one() {
        let mut ctx = TestCtx::new(6);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        // Nothing has ever been heard: zero.
        assert_eq!(Mac::<TestCtx>::cbr(&mac, node, CH, 0), 0.0);
        ctx.set_now(500_000_000);
        Mac::on_cca(&mut mac, &mut ctx, node, CH, CcaState::Idle);
        assert_eq!(Mac::<TestCtx>::cbr(&mac, node, CH, ctx.now()), 0.0);

        // A saturated channel: busy for the whole window.
        let mut saturated = EdcaOcbMac::new();
        saturated.note_busy(node, CH, 0, 1_000_000_000);
        let meter_cbr = Mac::<TestCtx>::cbr(&saturated, node, CH, 1_000_000_000);
        assert!((meter_cbr - 1.0).abs() < 1e-12, "{meter_cbr}");

        // Half busy: one 50 ms interval inside the 100 ms window.
        let mut half = EdcaOcbMac::new();
        half.note_busy(node, CH, 950_000_000, 1_000_000_000);
        let half_cbr = Mac::<TestCtx>::cbr(&half, node, CH, 1_000_000_000);
        assert!((half_cbr - 0.5).abs() < 1e-12, "{half_cbr}");

        // A channel filled with 584 µs frames back to back approaches one.
        let mut loaded = EdcaOcbMac::new();
        let air = air_time(400, Mcs::R6Qpsk12).as_nanos();
        let mut t = 0;
        while t + air <= 100_000_000 {
            loaded.note_busy(node, CH, t, t + air);
            t += air;
        }
        let loaded_cbr = Mac::<TestCtx>::cbr(&loaded, node, CH, 100_000_000);
        assert!(loaded_cbr > 0.99, "{loaded_cbr}");
    }

    /// The CBR a MAC reports is the load **now**, not the load at the last instant its
    /// meter happened to hear something.
    ///
    /// `Mac::cbr` used to end the window at `CbrMeter::last_known()`, so a node busy from
    /// 0 to 50 ms and silent ever since reported 1.000000 for the rest of the run, and a
    /// light load of ten 584 µs frames in the first 100 ms reported 0.064471 for ever
    /// instead of decaying to zero. Every `Dcc` model consumes this through `on_cbr`, so
    /// such a node was pinned at `δ_min` = 0.0006 — one frame per second — permanently.
    #[test]
    fn a_channel_that_falls_silent_decays_to_zero() {
        let node = NodeId::new(0);

        // Saturated for the first 50 ms, then nothing.
        let mut mac = EdcaOcbMac::new();
        mac.note_busy(node, CH, 0, 50_000_000);
        // Inside the window it reads what it should.
        assert!((Mac::<TestCtx>::cbr(&mac, node, CH, 50_000_000) - 1.0).abs() < 1e-12);
        // Half the window later, half of it is still inside.
        let at_100 = Mac::<TestCtx>::cbr(&mac, node, CH, 100_000_000);
        assert!((at_100 - 0.5).abs() < 1e-12, "{at_100}");
        // Once the busy period has fallen out of the 100 ms window, zero — and it stays
        // zero however long the node is quiet for.
        for now in [150_000_000, 1_000_000_000, 600_000_000_000] {
            let cbr = Mac::<TestCtx>::cbr(&mac, node, CH, now);
            assert_eq!(cbr, 0.0, "{cbr} at {now} ns");
        }
        // The stale reading this replaced.
        assert_eq!(
            mac.state_ref(node, CH).expect("state").cbr.last_known(),
            50_000_000
        );

        // The validator's light-load case: ten 584 µs frames in the first 100 ms.
        let mut light = EdcaOcbMac::new();
        let air = air_time(400, Mcs::R6Qpsk12).as_nanos();
        for i in 0..10u64 {
            light.note_busy(node, CH, i * 10_000_000, i * 10_000_000 + air);
        }
        let loaded = Mac::<TestCtx>::cbr(&light, node, CH, 100_000_000);
        assert!((loaded - 0.058_4).abs() < 1e-9, "{loaded}");
        assert_eq!(Mac::<TestCtx>::cbr(&light, node, CH, 1_000_000_000), 0.0);
    }

    #[test]
    fn overlapping_busy_intervals_cannot_push_the_cbr_above_one() {
        let mut meter = CbrMeter::new();
        // Three receptions that all overlap: the medium is busy once.
        meter.note_busy(0, 60_000_000);
        meter.note_busy(10_000_000, 70_000_000);
        meter.note_busy(20_000_000, 50_000_000);
        let cbr = meter.cbr(100_000_000);
        assert!((cbr - 0.7).abs() < 1e-12, "{cbr}");
        assert!(cbr <= 1.0);
    }

    #[test]
    fn the_cbr_window_forgets_what_falls_out_of_it() {
        let mut meter = CbrMeter::new();
        meter.note_busy(0, 50_000_000);
        assert!((meter.cbr(100_000_000) - 0.5).abs() < 1e-12);
        // Half a second later that interval is long gone.
        assert_eq!(meter.cbr(500_000_000), 0.0);
        assert_eq!(meter.window(), timing::T_CBR);
    }

    #[test]
    fn mac_airtime_accounting_sums_to_the_transmitted_time() {
        // The MAC half of invariant I-R3: the air time it grants equals the air time of
        // the frames it granted, frame for frame.
        let mut ctx = TestCtx::new(7);
        let mut mac = EdcaOcbMac::new();
        let node = NodeId::new(0);
        let sizes = [100u32, 300, 400, 1_000, 2_304];
        let mut expected = 0u64;
        let mut granted = 0u64;
        for (i, bytes) in sizes.into_iter().enumerate() {
            ctx.set_now(1_000_000 * (i as u64 + 1));
            let now = ctx.now();
            Mac::enqueue(
                &mut mac,
                &mut ctx,
                node,
                sdu(bytes, now),
                AccessCategory::Vo,
            )
            .expect("queued");
            let grant = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
            expected += air_time(grant.sdu.frame.bytes, grant.sdu.frame.mcs).as_nanos();
            granted += 1;
        }
        assert_eq!(granted, sizes.len() as u64);
        assert_eq!(mac.granted_airtime_ns(node, CH), expected);
        assert_eq!(
            mac.granted(node, CH, AccessCategory::Vo),
            sizes.len() as u64
        );
    }

    #[test]
    fn a_full_queue_drops_and_says_which_queue() {
        let mut ctx = TestCtx::new(8);
        let mut mac = EdcaOcbMac::new().with_queue_depth(2);
        let node = NodeId::new(0);
        for _ in 0..2 {
            Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Be)
                .expect("queued");
        }
        let err = Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Be)
            .expect_err("full");
        assert!(matches!(err, DropCause::QueueFull { depth: 2, .. }));
        assert_eq!(mac.dropped(node, CH), 1);
        // And an oversized frame is refused whatever the queue looks like.
        let err = Mac::enqueue(
            &mut mac,
            &mut ctx,
            node,
            sdu(timing::MAX_MSDU_BYTES + 1, 0),
            AccessCategory::Vo,
        )
        .expect_err("too large");
        assert!(matches!(err, DropCause::TooLarge { .. }));
    }

    #[test]
    fn the_slotted_mac_grants_at_its_slot() {
        let mut ctx = TestCtx::new(9);
        let mut mac = SlottedMac::new();
        let node = NodeId::new(0);
        Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Vo).expect("queued");
        let slot = mac.pending_slot(node, CH).expect("a slot was drawn");
        assert!(slot <= AccessCategory::Vo.cw_min());
        if slot > 0 {
            ctx.set_now(u64::from(slot) * timing::SLOT_TIME.as_nanos() - 1);
            assert!(
                Mac::poll(&mut mac, &mut ctx, node, CH).is_none(),
                "too early"
            );
        }
        ctx.set_now(u64::from(slot) * timing::SLOT_TIME.as_nanos());
        let grant = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
        assert_eq!(grant.backoff_slots, slot);
        assert_eq!(
            mac.granted_airtime_ns(node, CH),
            air_time(300, Mcs::R6Qpsk12).as_nanos()
        );
        assert_eq!(
            Mac::<TestCtx>::resource_model(&mac),
            ResourceModel::Csma { edca: false }
        );
        assert_eq!(Mac::<TestCtx>::tier(&mac), Tier::Medium);
    }

    /// The transmission instant is the slot the backoff computed, and the engine can
    /// find out when that is: nodes that drew *different* slots are granted at
    /// *different* instants, whatever cadence the engine polls on.
    ///
    /// `TxGrant.at` used to be `ctx.now()` in both MACs and the `Mac` trait offered no
    /// way to ask when to poll, so a coarse poll found every node past due and granted
    /// them all in the same instant — which erases the medium tier's only collision
    /// mechanism, since two nodes 13 µs apart then collide anyway.
    #[test]
    fn a_grant_is_at_the_slot_the_backoff_computed_not_at_the_poll() {
        let mut ctx = TestCtx::new(23);
        let mut mac = SlottedMac::new();
        let slot_ns = timing::SLOT_TIME.as_nanos();

        // Eight nodes queue at t = 0 and the engine polls late and coarsely, at 10 ms.
        let mut drawn = BTreeMap::new();
        for id in 0..8u32 {
            let node = NodeId::new(id);
            Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Vo)
                .expect("queued");
            let slot = mac.pending_slot(node, CH).expect("slot");
            // The engine can schedule its timer exactly, before it polls anything.
            assert_eq!(
                Mac::<TestCtx>::next_poll_at(&mac, node, CH),
                Some(u64::from(slot) * slot_ns)
            );
            drawn.insert(id, slot);
        }
        ctx.set_now(10_000_000);
        let mut granted_at = BTreeMap::new();
        for id in 0..8u32 {
            let node = NodeId::new(id);
            let g = Mac::poll(&mut mac, &mut ctx, node, CH).expect("granted");
            assert_eq!(g.backoff_slots, drawn[&id]);
            granted_at.insert(id, g.at);
        }
        // Two nodes share an instant exactly when they drew the same slot — not because
        // they were polled together.
        for a in 0..8u32 {
            for b in 0..8u32 {
                assert_eq!(
                    granted_at[&a] == granted_at[&b],
                    drawn[&a] == drawn[&b],
                    "nodes {a} and {b}: slots {} and {}, instants {} and {}",
                    drawn[&a],
                    drawn[&b],
                    granted_at[&a],
                    granted_at[&b]
                );
                assert_eq!(granted_at[&a], u64::from(drawn[&a]) * slot_ns);
            }
        }
        let distinct: std::collections::BTreeSet<SimTime> =
            granted_at.values().copied().collect();
        assert!(distinct.len() > 1, "every node granted in one instant: {granted_at:?}");

        // EDCA answers the same question from its own state: AIFS plus the drawn slots,
        // and nothing at all when the queue is empty or the medium is busy.
        let mut ctx = TestCtx::new(24);
        let mut edca = EdcaOcbMac::new();
        let node = NodeId::new(0);
        assert_eq!(Mac::<TestCtx>::next_poll_at(&edca, node, CH), None);
        ctx.set_now(1_000_000);
        Mac::on_cca(&mut edca, &mut ctx, node, CH, CcaState::Busy { energy_dbm: -60.0 });
        let at = ctx.now();
        Mac::enqueue(&mut edca, &mut ctx, node, sdu(300, at), AccessCategory::Vo).expect("queued");
        assert_eq!(
            Mac::<TestCtx>::next_poll_at(&edca, node, CH),
            None,
            "frozen on a busy medium"
        );
        ctx.set_now(2_000_000);
        Mac::on_cca(&mut edca, &mut ctx, node, CH, CcaState::Idle);
        let slots = edca
            .backoff(node, CH, AccessCategory::Vo)
            .expect("armed")
            .remaining;
        let expected =
            2_000_000 + AccessCategory::Vo.aifs().as_nanos() + u64::from(slots) * slot_ns;
        assert_eq!(
            Mac::<TestCtx>::next_poll_at(&edca, node, CH),
            Some(expected)
        );
        // Polling at exactly that instant grants at exactly that instant.
        ctx.set_now(expected);
        let g = Mac::poll(&mut edca, &mut ctx, node, CH).expect("granted");
        assert_eq!(g.at, expected);
        assert_eq!(Mac::<TestCtx>::next_poll_at(&edca, node, CH), None);
    }

    #[test]
    fn two_nodes_drawing_the_same_slot_is_what_a_collision_is() {
        // The medium tier's collision mechanism: the MAC does not detect it, it simply
        // grants both frames in the same slot and the PHY sees two overlapping arrivals.
        let mut ctx = TestCtx::new(10);
        let mut mac = SlottedMac::new();
        let mut slots = BTreeMap::new();
        for id in 0..8u32 {
            let node = NodeId::new(id);
            Mac::enqueue(&mut mac, &mut ctx, node, sdu(300, 0), AccessCategory::Vo)
                .expect("queued");
            slots.insert(id, mac.pending_slot(node, CH).expect("slot"));
        }
        // With CWmin = 3 and eight nodes, at least two must share a slot.
        let distinct: std::collections::BTreeSet<u32> = slots.values().copied().collect();
        assert!(distinct.len() < slots.len(), "{slots:?}");
    }

    #[test]
    fn the_resource_model_and_tier_are_reported() {
        let edca = EdcaOcbMac::new();
        assert_eq!(
            Mac::<TestCtx>::resource_model(&edca),
            ResourceModel::Csma { edca: true }
        );
        assert_eq!(Mac::<TestCtx>::tier(&edca), Tier::High);
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        for card in [edca_card(), slotted_card()] {
            card.validate().expect("card validates");
            card.check_api_version().expect("api version");
            registry.register(card).expect("registers");
        }
        assert!(registry.contains(EdcaOcbMac::ID));
        assert!(registry.contains(SlottedMac::ID));
    }

    #[test]
    fn a_mac_can_be_used_as_a_trait_object() {
        let mut ctx = TestCtx::new(11);
        let mut mac: Box<dyn Mac<TestCtx>> = Box::new(EdcaOcbMac::new());
        let node = NodeId::new(0);
        ctx.set_now(1_000_000);
        let now = ctx.now();
        mac.enqueue(&mut ctx, node, sdu(300, now), AccessCategory::Vo)
            .expect("queued");
        assert!(mac.poll(&mut ctx, node, CH).is_some());
        assert_eq!(mac.id(), EdcaOcbMac::ID);
    }
}
