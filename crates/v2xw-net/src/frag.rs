//! The `Fragmenter` seam of 03-interfaces.md §5 and the four strategies of
//! 04-models.md §7.3.
//!
//! Neither WSMP nor GeoNetworking has a fragmentation field ([`crate::wsmp`],
//! [`crate::gn`]), so everything that splits an oversize message happens **above** the
//! network layer. That is this seam. The four models:
//!
//! | Model | What it does |
//! |---|---|
//! | [`none`] | refuses an oversize SDU with [`DropCause::Mtu`]; the default for BSM and CAM |
//! | [`facilities`] | ETSI facilities-layer segmentation: independently interpretable segments, so losing one loses only its content |
//! | [`cert_cycle`] | the NDSS 2024 Partially-Hybrid scheme: the hybrid certificate spread over the first α messages of each five-message certificate cycle |
//! | [`generic`] | a generic split with a four-byte fragment header, a reassembly buffer, a timeout and out-of-order tolerance |
//!
//! Every card states its reassembly timeout and its loss amplification, which is invariant
//! I-N2; [`Fragmenter::reassembly_timeout`] and [`Fragmenter::loss_amplification`] are the
//! runtime half of that.
//!
//! # Two deviations from the §5 sketch, and why
//!
//! 03-interfaces.md sketches `fn fragment(&self, sdu_bytes: u32, mtu: u32) ->
//! Vec<FragmentDesc>`. The implemented signature is
//! [`Fragmenter::fragment`]`(&self, sdu: SduId, sdu_bytes: u32, mtu: u32) ->
//! Result<Vec<FragmentDesc>, DropCause>`:
//!
//! * **The SDU id is a parameter** because a [`FragmentDesc`] has to identify the SDU it
//!   belongs to — reassembly is keyed per `(sender, SDU)` (04-models.md §7.3) — and a
//!   descriptor that could not name its SDU would have to be paired with one by every
//!   caller.
//! * **The return is a `Result`** because 04-models.md §7.3 requires `fragmenter/none` to
//!   reject an oversize SDU *with a cause* (`DropCause::Mtu`), and a bare `Vec` has nowhere
//!   to put one. Two more models refuse configurations the standard cannot express
//!   ([`DropCause::TooManyFragments`], [`DropCause::CycleTooShort`]).
//!
//! The context type parameter is the same device `v2xw-msg`'s `MessageGenerator` uses; see
//! [`crate::netlayer::NetLayer`] for why.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::ctx::{Ctx, Record, Visibility};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::amplification::{FragmentLoss, SduLossModel, sdu_loss};
use crate::error::DropCause;

pub mod cert_cycle;
pub mod facilities;
pub mod generic;
pub mod none;

/// The largest fragment index a [`ReassemblyBuffer`] tracks, plus one.
///
/// The buffer keeps arrival state in a fixed 256-bit set, which covers indices `0..=255` —
/// the whole range a one-octet fragment count can express ([`generic`]'s header) with no
/// allocation per open SDU. A strategy with a wider count field would need a different
/// buffer, and [`ReassemblyBuffer::accept`] refuses the configuration rather than silently
/// truncating it.
pub const REASSEMBLY_MAX_FRAGMENTS: u16 = 256;

/// What kind of piece a fragment is — which decides what its loss costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FragmentKind {
    /// The SDU was not fragmented: one descriptor, index 0 of 1.
    Whole,
    /// An independently interpretable facilities-layer segment: a complete, separately
    /// signed message carrying part of the content (04-models.md §7.3).
    Segment,
    /// A piece that means nothing on its own: the receiver needs every sibling.
    Piece,
    /// A fragment of a hybrid certificate, carried inside an otherwise ordinary message
    /// (04-models.md §7.3, the Partially-Hybrid design).
    CertFragment,
}

impl FragmentKind {
    /// Whether this piece can be used without its siblings.
    pub const fn independently_interpretable(self) -> bool {
        matches!(self, FragmentKind::Whole | FragmentKind::Segment)
    }
}

/// One fragment of one SDU: what it carries and what it costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentDesc {
    /// The SDU this fragment belongs to.
    pub sdu: SduId,
    /// The fragment's index, `0..count`.
    pub index: u16,
    /// How many fragments the SDU was split into. `1` means it was not split.
    pub count: u16,
    /// The fragmentation overhead this fragment carries, bytes — the strategy's own header,
    /// not the network layer's.
    pub header_bytes: u32,
    /// The SDU bytes this fragment carries.
    pub payload_bytes: u32,
    /// What kind of piece it is.
    pub kind: FragmentKind,
}

impl FragmentDesc {
    /// A whole, unfragmented SDU.
    pub const fn whole(sdu: SduId, payload_bytes: u32) -> Self {
        Self {
            sdu,
            index: 0,
            count: 1,
            header_bytes: 0,
            payload_bytes,
            kind: FragmentKind::Whole,
        }
    }

    /// Overhead plus payload: what the network layer below is asked to carry.
    pub const fn total_bytes(&self) -> u32 {
        self.header_bytes.saturating_add(self.payload_bytes)
    }

    /// True if this descriptor is an unsplit SDU.
    pub const fn is_whole(&self) -> bool {
        self.count == 1
    }
}

/// What a reassembly step concluded.
///
/// `Complete` means "this much content is deliverable now". For a strategy that needs every
/// fragment that happens once, when the last one arrives. For [`facilities`], where each
/// segment is a complete message, it happens **once per segment** — which is the whole
/// point of that strategy, and is why the variant carries how many segments contributed
/// rather than a bare id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
#[non_exhaustive]
pub enum ReassemblyOutcome {
    /// Some fragments are here, more are needed.
    Pending {
        /// The SDU.
        sdu: SduId,
        /// How many distinct fragments have arrived.
        have: u16,
        /// How many are needed.
        want: u16,
    },
    /// Content is deliverable: `bytes` of SDU payload from `segments` fragments.
    Complete {
        /// The SDU.
        sdu: SduId,
        /// SDU payload bytes now deliverable.
        bytes: u32,
        /// How many fragments contributed them.
        segments: u16,
    },
    /// The fragment, or the whole set, was discarded for exactly one reason.
    Failed {
        /// The SDU.
        sdu: SduId,
        /// Why.
        cause: DropCause,
    },
    /// A fragment of an SDU this receiver has **already delivered**.
    ///
    /// Not a loss and not a reassembly: the content was delivered when the set completed,
    /// and nothing more is deliverable now. It is a distinct variant rather than a repeat
    /// of [`ReassemblyOutcome::Complete`] because a caller that sums `bytes` over
    /// `Complete` would otherwise count one SDU's payload twice.
    Duplicate {
        /// The SDU.
        sdu: SduId,
        /// How many fragments it was delivered from.
        segments: u16,
    },
    /// The reassembly timer ran out with fragments still missing.
    Expired {
        /// The SDU.
        sdu: SduId,
        /// How many fragments had arrived.
        have: u16,
        /// How many were needed.
        want: u16,
        /// How many SDU payload bytes are discarded with the set.
        bytes: u32,
    },
}

impl ReassemblyOutcome {
    /// The SDU this outcome is about.
    pub const fn sdu(&self) -> SduId {
        match *self {
            ReassemblyOutcome::Pending { sdu, .. }
            | ReassemblyOutcome::Complete { sdu, .. }
            | ReassemblyOutcome::Duplicate { sdu, .. }
            | ReassemblyOutcome::Failed { sdu, .. }
            | ReassemblyOutcome::Expired { sdu, .. } => sdu,
        }
    }

    /// The stable label this outcome is recorded under on the `net.frag` channel.
    pub const fn label(&self) -> &'static str {
        match self {
            ReassemblyOutcome::Pending { .. } => "pending",
            ReassemblyOutcome::Complete { .. } => "complete",
            ReassemblyOutcome::Duplicate { .. } => "duplicate",
            ReassemblyOutcome::Failed { .. } => "failed",
            ReassemblyOutcome::Expired { .. } => "expired",
        }
    }

    /// True if the SDU's content will never be delivered.
    pub const fn is_loss(&self) -> bool {
        matches!(
            self,
            ReassemblyOutcome::Failed { .. } | ReassemblyOutcome::Expired { .. }
        )
    }
}

/// The `net.frag` record (03-interfaces.md §14: "t, node, sdu id, fragments, outcome").
///
/// [`Visibility::Node`]: everything in it is the receiving node's own bookkeeping. The
/// **sender's identity is deliberately absent**. A reassembly buffer is keyed per peer, and
/// at these tiers the engine keys it by the transmitter's [`NodeId`] — which is ground
/// truth, since a receiver knows only a pseudonym digest. Putting it in the record would
/// make the channel `NodeAndGt` and would hand a detector or a dataset exporter the
/// transmitter identity that the recorder's own rules exist to withhold
/// ([`Visibility::allowed_on_node_channel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FragRecord {
    /// The receiving node's own clock, nanoseconds.
    pub t_ns: SimTime,
    /// The receiving node.
    pub node: NodeId,
    /// The SDU being reassembled.
    pub sdu: SduId,
    /// How many fragments the SDU was split into.
    pub fragments: u16,
    /// How many distinct fragments had arrived when this outcome was reached.
    pub have: u16,
    /// SDU payload bytes delivered (on `complete`) or discarded (on `expired`).
    pub bytes: u32,
    /// The outcome's stable label ([`ReassemblyOutcome::label`]).
    pub outcome: &'static str,
}

impl Record for FragRecord {
    const CHANNEL: &'static str = "net.frag";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// Whether an outcome deserves a `net.frag` record.
///
/// **A fragmentation event or a loss, never an ordinary delivery.** Every fragmenter sits in
/// the path of every message a node receives, and most of those messages were never
/// fragmented: a BSM at 10 Hz from each of a few hundred neighbours passes through
/// `fragmenter/none` as one whole descriptor. Emitting a `complete` record for each would
/// put tens of thousands of records per simulated second on a channel whose subject is
/// fragmentation, and would drown the events that matter in it. So a record goes out when
/// the SDU was actually split (`count > 1`) or when content was lost, and an unfragmented
/// SDU passing through leaves no trace on this channel — the message itself is already on
/// `node.tx` and `phy.rx`.
pub(crate) fn should_record(frag: &FragmentDesc, outcome: &ReassemblyOutcome) -> bool {
    !frag.is_whole() || outcome.is_loss()
}

impl FragRecord {
    /// The record for one outcome at one node.
    pub const fn new(t_ns: SimTime, node: NodeId, outcome: &ReassemblyOutcome) -> Self {
        let (sdu, fragments, have, bytes) = match *outcome {
            ReassemblyOutcome::Pending { sdu, have, want } => (sdu, want, have, 0),
            ReassemblyOutcome::Complete {
                sdu,
                bytes,
                segments,
            } => (sdu, segments, segments, bytes),
            // Zero bytes: the payload was recorded when the set completed, and a record
            // that repeated it would double the delivered total on this channel.
            ReassemblyOutcome::Duplicate { sdu, segments } => (sdu, segments, segments, 0),
            ReassemblyOutcome::Failed { sdu, .. } => (sdu, 0, 0, 0),
            ReassemblyOutcome::Expired {
                sdu,
                have,
                want,
                bytes,
            } => (sdu, want, have, bytes),
        };
        Self {
            t_ns,
            node,
            sdu,
            fragments,
            have,
            bytes,
            outcome: outcome.label(),
        }
    }
}

/// An application-layer fragmentation strategy (03-interfaces.md §5, 04-models.md §7.3).
pub trait Fragmenter<C>: Model
where
    C: Ctx + ?Sized,
{
    /// Splits an SDU of `sdu_bytes` bytes for a link whose effective MTU is `mtu`.
    ///
    /// The descriptors come back in index order and their payloads sum to exactly
    /// `sdu_bytes`; the strategy's own overhead is on each descriptor's
    /// [`FragmentDesc::header_bytes`].
    ///
    /// # Errors
    /// The [`DropCause`] the strategy reports for an SDU it cannot carry — `Mtu` for
    /// [`none`], `TooManyFragments` for [`generic`], `CycleTooShort` for [`cert_cycle`].
    fn fragment(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause>;

    /// Takes one arriving fragment at `rx`, sent by `from`.
    ///
    /// The return value is always about **this** fragment's SDU. A strategy with a timeout
    /// also retires sets whose timer ran out; those outcomes come back from
    /// [`Fragmenter::expire`], and each one is recorded on `net.frag` as it is retired, so
    /// nothing is lost by the split.
    fn reassemble(
        &mut self,
        ctx: &mut C,
        rx: NodeId,
        frag: &FragmentDesc,
        from: NodeId,
    ) -> ReassemblyOutcome;

    /// The strategy's own per-fragment overhead, bytes.
    fn overhead_bytes(&self) -> u32;

    /// The reassembly timeout, or `None` for a strategy that keeps no reassembly state.
    ///
    /// Invariant I-N2 requires every card to state this; the method is so a caller does not
    /// have to parse the card to schedule the timer.
    fn reassembly_timeout(&self) -> Option<Duration>;

    /// Whether losing one fragment loses the whole SDU.
    ///
    /// `true` for every strategy whose fragments are meaningless alone; `false` for
    /// [`facilities`], whose segments are complete messages.
    fn amplifies_loss(&self) -> bool {
        true
    }

    /// The loss the per-fragment probabilities imply for the SDU (04-models.md §7.4).
    ///
    /// Defaulted in terms of [`crate::amplification::sdu_loss`] and
    /// [`Fragmenter::amplifies_loss`], so no strategy can implement the formula a second
    /// (and different) time.
    fn loss_amplification(&self, per_fragment: &[FragmentLoss]) -> SduLossModel {
        sdu_loss(per_fragment, self.amplifies_loss())
    }

    /// Retires every reassembly whose timer has run out at `now`, in `(peer, SDU)` order.
    ///
    /// Defaulted to "nothing to retire" for the strategies that keep no buffer. The engine
    /// calls it on a timer; [`Fragmenter::reassembly_timeout`] is the interval to use.
    fn expire(&mut self, _ctx: &mut C, _rx: NodeId, _now: SimTime) -> Vec<ReassemblyOutcome> {
        Vec::new()
    }
}

// =========================================================================================
// The shared reassembly buffer
// =========================================================================================

/// Which fragment indices of one SDU have arrived: a fixed 256-bit set.
///
/// Fixed size rather than a `Vec<bool>` so an open SDU costs no allocation, and 256 bits
/// because that is the whole range a one-octet fragment count can express.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct IndexSet([u64; 4]);

impl IndexSet {
    /// Marks `index` as arrived; returns `true` if it was not already set.
    fn mark(&mut self, index: u16) -> bool {
        let (word, bit) = (usize::from(index) / 64, usize::from(index) % 64);
        let mask = 1u64 << bit;
        let was = self.0[word] & mask != 0;
        self.0[word] |= mask;
        !was
    }
}

/// One SDU this receiver has delivered, remembered for a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Completed {
    /// How many fragments it was delivered from, so a late fragment claiming a different
    /// count is still caught as a count mismatch.
    segments: u16,
    /// When the memory of it is dropped: one timeout after completion, the same window a
    /// partly received set gets, because it bounds how late a sender's repetition can be
    /// and still be a repetition.
    deadline: SimTime,
}

/// One partly received SDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    /// How many fragments the first arrival declared.
    want: u16,
    /// How many distinct fragments have arrived.
    have: u16,
    /// Which indices have arrived.
    seen: IndexSet,
    /// SDU payload bytes accumulated.
    bytes: u32,
    /// When the set is abandoned.
    deadline: SimTime,
}

/// A reassembly buffer per `(peer, SDU)` with a timeout and out-of-order tolerance
/// (04-models.md §7.3).
///
/// Shared by [`generic`] and [`cert_cycle`], which differ in how they *split* an SDU and
/// not in how a receiver puts one back together.
///
/// # The rules it implements
///
/// * **Out of order is normal.** Fragments are tracked in a set of indices, never in a
///   sequence, so any arrival order reassembles identically.
/// * **A duplicate is tolerated, not an error.** Its bytes are counted once. A repeated
///   fragment is what a sender's own repetition or a captured retransmission looks like,
///   and refusing the SDU for it would invent a loss. This holds **after** the set
///   completes as well: a completed `(peer, SDU)` is remembered for one timeout and a
///   fragment that arrives against it answers [`ReassemblyOutcome::Duplicate`]. Without
///   that memory the entry was gone, the late fragment fell through to the "no entry" arm
///   and opened a fresh reassembly that could never finish — so a delivered SDU was
///   retired a timeout later as an `Expired` loss, on the very metric 04-models.md §7.4
///   exists to expose, and the repeat held a buffer slot until it was.
/// * **The timer starts at the first fragment** and is not extended by later ones, so the
///   timeout bounds the set's whole lifetime — which is the condition assumption 4 of
///   04-models.md §7.4 is about.
/// * **A full buffer refuses the new SDU** rather than evicting an older one, so the loss is
///   attributed to the SDU that could not be admitted and the decision does not depend on
///   an eviction order.
/// * **Expiry is drained, not returned.** [`ReassemblyBuffer::accept`] retires stale sets
///   before it looks at the arriving fragment, and its return value is always about that
///   fragment's own SDU; the retired sets come back from [`ReassemblyBuffer::take_expired`].
///
/// # Keying
///
/// The key is `(from, sdu)` with the full [`SduId`], not the truncated id a fragment header
/// carries, so two SDUs can never be confused for one. [`generic`]'s two-octet id field is
/// modelled as *overhead*; the aliasing a real 16-bit field would allow after 65,536
/// outstanding SDUs from one peer is recorded as a limitation on its card and is unreachable
/// under the buffer's own cap.
///
/// At the `high` tier the key becomes the sender's pseudonym digest, which is what a
/// receiver actually observes; [`NodeId`] stands in for it here, and nothing derived from it
/// reaches a record (see [`FragRecord`]).
#[derive(Debug, Clone)]
pub struct ReassemblyBuffer {
    entries: BTreeMap<(NodeId, SduId), Entry>,
    /// `(peer, SDU)` sets completed within the last timeout, so a fragment that arrives
    /// after delivery is recognised instead of opening a new reassembly. Held apart from
    /// `entries` on purpose: a remembered completion must not consume a reassembly slot,
    /// which is what made a repeat after a completion refuse a genuinely new SDU.
    completed: BTreeMap<(NodeId, SduId), Completed>,
    expired: Vec<ReassemblyOutcome>,
    max_open: usize,
    timeout: Duration,
}

impl ReassemblyBuffer {
    /// A buffer holding at most `max_open` partly received SDUs, each for `timeout`.
    pub fn new(max_open: usize, timeout: Duration) -> Self {
        Self {
            entries: BTreeMap::new(),
            completed: BTreeMap::new(),
            expired: Vec::new(),
            max_open,
            timeout,
        }
    }

    /// The configured timeout.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The configured capacity, in partly received SDUs.
    pub const fn max_open(&self) -> usize {
        self.max_open
    }

    /// How many SDUs are partly received.
    ///
    /// Sets that have completed are **not** counted: they hold no reassembly state, only a
    /// key and a deadline, and they never refuse an arriving SDU.
    pub fn open(&self) -> usize {
        self.entries.len()
    }

    /// How many completed `(peer, SDU)` keys are still remembered.
    ///
    /// Exposed so a test can assert that the memory is bounded and is released on time,
    /// rather than taking it on trust.
    pub fn remembered(&self) -> usize {
        self.completed.len()
    }

    /// Takes one arriving fragment. See the type's documentation for the rules.
    pub fn accept(&mut self, now: SimTime, from: NodeId, frag: &FragmentDesc) -> ReassemblyOutcome {
        self.purge(now);

        let sdu = frag.sdu;
        if frag.count == 0 || frag.index >= frag.count {
            return ReassemblyOutcome::Failed {
                sdu,
                cause: DropCause::UnexpectedFragment {
                    index: frag.index,
                    count: frag.count,
                },
            };
        }
        if frag.count > REASSEMBLY_MAX_FRAGMENTS {
            return ReassemblyOutcome::Failed {
                sdu,
                cause: DropCause::TooManyFragments {
                    needed: u32::from(frag.count),
                    max: REASSEMBLY_MAX_FRAGMENTS,
                },
            };
        }
        // An unsplit SDU needs no state at all.
        if frag.count == 1 {
            return ReassemblyOutcome::Complete {
                sdu,
                bytes: frag.payload_bytes,
                segments: 1,
            };
        }

        let key = (from, sdu);
        match self.entries.get_mut(&key) {
            Some(entry) => {
                if entry.want != frag.count {
                    let had = entry.want;
                    self.entries.remove(&key);
                    return ReassemblyOutcome::Failed {
                        sdu,
                        cause: DropCause::FragmentCountMismatch {
                            had,
                            saw: frag.count,
                        },
                    };
                }
                if !entry.seen.mark(frag.index) {
                    // A duplicate: tolerated, and its bytes are not counted again.
                    return ReassemblyOutcome::Pending {
                        sdu,
                        have: entry.have,
                        want: entry.want,
                    };
                }
                entry.have += 1;
                entry.bytes = entry.bytes.saturating_add(frag.payload_bytes);
                if entry.have == entry.want {
                    let (bytes, segments) = (entry.bytes, entry.want);
                    self.entries.remove(&key);
                    self.remember(key, segments, now);
                    ReassemblyOutcome::Complete {
                        sdu,
                        bytes,
                        segments,
                    }
                } else {
                    ReassemblyOutcome::Pending {
                        sdu,
                        have: entry.have,
                        want: entry.want,
                    }
                }
            }
            None => {
                // A fragment of something already delivered: a sender's own repetition, or
                // a capture of one. Answered as a duplicate, not opened as a new set.
                if let Some(done) = self.completed.get(&key).copied() {
                    if done.segments == frag.count {
                        return ReassemblyOutcome::Duplicate {
                            sdu,
                            segments: done.segments,
                        };
                    }
                    // A repeat claiming a different count is malformed, exactly as it is
                    // while the set is still open; the memory is dropped with it so the
                    // receiver is not left arguing with itself about the same key.
                    self.completed.remove(&key);
                    return ReassemblyOutcome::Failed {
                        sdu,
                        cause: DropCause::FragmentCountMismatch {
                            had: done.segments,
                            saw: frag.count,
                        },
                    };
                }
                if self.entries.len() >= self.max_open {
                    return ReassemblyOutcome::Failed {
                        sdu,
                        cause: DropCause::ReassemblyBufferFull {
                            open: self.entries.len(),
                            max: self.max_open,
                        },
                    };
                }
                let mut seen = IndexSet::default();
                seen.mark(frag.index);
                self.entries.insert(
                    key,
                    Entry {
                        want: frag.count,
                        have: 1,
                        seen,
                        bytes: frag.payload_bytes,
                        deadline: self.timeout.after(now),
                    },
                );
                ReassemblyOutcome::Pending {
                    sdu,
                    have: 1,
                    want: frag.count,
                }
            }
        }
    }

    /// Retires every set whose deadline has passed and returns the outcomes, in
    /// `(peer, SDU)` order.
    pub fn expire(&mut self, now: SimTime) -> Vec<ReassemblyOutcome> {
        self.purge(now);
        self.take_expired()
    }

    /// Drains the outcomes of sets already retired by [`ReassemblyBuffer::accept`].
    pub fn take_expired(&mut self) -> Vec<ReassemblyOutcome> {
        core::mem::take(&mut self.expired)
    }

    /// Remembers a completed `(peer, SDU)` for one timeout.
    ///
    /// Bounded by `max_open`, so the memory cannot outgrow the buffer it belongs to. When
    /// it is full the entry with the earliest deadline goes — and, among equal deadlines,
    /// the lowest `(peer, SDU)` key, because `BTreeMap` iterates in key order and the
    /// choice must not depend on insertion history.
    fn remember(&mut self, key: (NodeId, SduId), segments: u16, now: SimTime) {
        if self.completed.len() >= self.max_open
            && !self.completed.contains_key(&key)
            && let Some(oldest) = self
                .completed
                .iter()
                .min_by_key(|(k, c)| (c.deadline, **k))
                .map(|(k, _)| *k)
        {
            self.completed.remove(&oldest);
        }
        self.completed.insert(
            key,
            Completed {
                segments,
                deadline: self.timeout.after(now),
            },
        );
    }

    /// Moves every set whose deadline has passed into the expired queue.
    ///
    /// `BTreeMap` iteration is in key order, so the queue's order is a function of the
    /// `(peer, SDU)` ids and not of insertion history — which is what makes the emitted
    /// records reproducible.
    fn purge(&mut self, now: SimTime) {
        // A completed key is forgotten silently: its content was delivered, so its passing
        // is not an outcome and emits no record.
        self.completed.retain(|_, c| c.deadline > now);
        if self.entries.is_empty() {
            return;
        }
        let stale: Vec<(NodeId, SduId)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.deadline <= now)
            .map(|(k, _)| *k)
            .collect();
        for key in stale {
            if let Some(entry) = self.entries.remove(&key) {
                self.expired.push(ReassemblyOutcome::Expired {
                    sdu: key.1,
                    have: entry.have,
                    want: entry.want,
                    bytes: entry.bytes,
                });
            }
        }
    }
}

/// Splits `total` bytes into `n` as-equal-as-possible parts that sum to exactly `total`.
///
/// The first `total % n` parts get one byte more, so the parts are within one byte of each
/// other and the sum is exact — the property every fragmenter's tests assert. `n == 0`
/// yields no parts.
pub(crate) fn split_equal(total: u32, n: u16) -> Vec<u32> {
    if n == 0 {
        return Vec::new();
    }
    let n32 = u32::from(n);
    let base = total / n32;
    let extra = total % n32;
    (0..n32)
        .map(|i| if i < extra { base + 1 } else { base })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::time::NS_PER_MS;

    fn piece(sdu: u32, index: u16, count: u16, payload: u32) -> FragmentDesc {
        FragmentDesc {
            sdu: SduId::new(sdu),
            index,
            count,
            header_bytes: 4,
            payload_bytes: payload,
            kind: FragmentKind::Piece,
        }
    }

    #[test]
    fn split_equal_sums_exactly_and_stays_within_one_byte() {
        for (total, n) in [(1_000u32, 3u16), (858, 4), (7, 5), (0, 3), (255, 255)] {
            let parts = split_equal(total, n);
            assert_eq!(parts.len(), usize::from(n));
            assert_eq!(parts.iter().sum::<u32>(), total, "total {total} into {n}");
            let (min, max) = (*parts.iter().min().unwrap(), *parts.iter().max().unwrap());
            assert!(max - min <= 1, "{parts:?}");
        }
        assert!(split_equal(100, 0).is_empty());
    }

    #[test]
    fn an_unsplit_sdu_needs_no_buffer_state() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        let out = buf.accept(0, NodeId::new(1), &FragmentDesc::whole(SduId::new(9), 300));
        assert_eq!(
            out,
            ReassemblyOutcome::Complete {
                sdu: SduId::new(9),
                bytes: 300,
                segments: 1
            }
        );
        assert_eq!(buf.open(), 0);
    }

    #[test]
    fn fragments_reassemble_in_and_out_of_order() {
        let timeout = Duration::from_millis(1_000);
        let peer = NodeId::new(2);
        let orders: [[u16; 3]; 4] = [[0, 1, 2], [2, 1, 0], [1, 0, 2], [2, 0, 1]];
        for order in orders {
            let mut buf = ReassemblyBuffer::new(4, timeout);
            let mut outcomes = Vec::new();
            for (step, index) in order.iter().enumerate() {
                let t = step as u64 * 10 * NS_PER_MS;
                outcomes.push(buf.accept(t, peer, &piece(5, *index, 3, 100)));
            }
            assert!(
                matches!(
                    outcomes[0],
                    ReassemblyOutcome::Pending {
                        have: 1,
                        want: 3,
                        ..
                    }
                ),
                "{order:?}: {outcomes:?}"
            );
            assert!(matches!(
                outcomes[1],
                ReassemblyOutcome::Pending {
                    have: 2,
                    want: 3,
                    ..
                }
            ));
            assert_eq!(
                outcomes[2],
                ReassemblyOutcome::Complete {
                    sdu: SduId::new(5),
                    bytes: 300,
                    segments: 3
                },
                "arrival order {order:?} must not change the result"
            );
            assert_eq!(buf.open(), 0, "a completed set leaves no state behind");
        }
    }

    #[test]
    fn a_duplicate_fragment_is_tolerated_and_counted_once() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        let peer = NodeId::new(2);
        buf.accept(0, peer, &piece(5, 0, 2, 100));
        let dup = buf.accept(NS_PER_MS, peer, &piece(5, 0, 2, 100));
        assert_eq!(
            dup,
            ReassemblyOutcome::Pending {
                sdu: SduId::new(5),
                have: 1,
                want: 2
            }
        );
        let done = buf.accept(2 * NS_PER_MS, peer, &piece(5, 1, 2, 100));
        assert_eq!(
            done,
            ReassemblyOutcome::Complete {
                sdu: SduId::new(5),
                bytes: 200,
                segments: 2
            },
            "the duplicate's bytes were not counted twice"
        );
    }

    #[test]
    fn a_set_that_times_out_is_retired_with_its_byte_count() {
        let timeout = Duration::from_millis(1_000);
        let mut buf = ReassemblyBuffer::new(4, timeout);
        let peer = NodeId::new(2);
        buf.accept(0, peer, &piece(5, 0, 3, 100));
        buf.accept(100 * NS_PER_MS, peer, &piece(5, 1, 3, 100));

        // One nanosecond before the deadline nothing is retired…
        assert!(buf.expire(1_000 * NS_PER_MS - 1).is_empty());
        assert_eq!(buf.open(), 1);
        // …and at the deadline the set goes, carrying what had arrived.
        assert_eq!(
            buf.expire(1_000 * NS_PER_MS),
            vec![ReassemblyOutcome::Expired {
                sdu: SduId::new(5),
                have: 2,
                want: 3,
                bytes: 200,
            }]
        );
        assert_eq!(buf.open(), 0);
        assert!(buf.expire(2_000 * NS_PER_MS).is_empty());
    }

    /// The timer starts at the first fragment and is not extended, so the timeout bounds the
    /// set's whole lifetime — assumption 4 of 04-models.md §7.4.
    #[test]
    fn a_later_fragment_does_not_extend_the_deadline() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        let peer = NodeId::new(2);
        buf.accept(0, peer, &piece(5, 0, 3, 100));
        buf.accept(900 * NS_PER_MS, peer, &piece(5, 1, 3, 100));
        assert_eq!(buf.expire(1_000 * NS_PER_MS).len(), 1);
    }

    /// Accepting a fragment after the deadline retires the stale set *and* starts a fresh
    /// one, so the late fragment is not lost as well.
    #[test]
    fn a_fragment_arriving_after_the_timeout_starts_a_new_set() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        let peer = NodeId::new(2);
        buf.accept(0, peer, &piece(5, 0, 2, 100));
        let out = buf.accept(1_500 * NS_PER_MS, peer, &piece(5, 1, 2, 100));
        assert_eq!(
            out,
            ReassemblyOutcome::Pending {
                sdu: SduId::new(5),
                have: 1,
                want: 2
            },
            "the arriving fragment opens a new set"
        );
        assert_eq!(
            buf.take_expired(),
            vec![ReassemblyOutcome::Expired {
                sdu: SduId::new(5),
                have: 1,
                want: 2,
                bytes: 100,
            }],
            "and the stale one is reported"
        );
    }

    #[test]
    fn an_inconsistent_fragment_set_is_refused() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        let peer = NodeId::new(2);
        // An index outside the count.
        assert_eq!(
            buf.accept(0, peer, &piece(1, 3, 3, 100)),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(1),
                cause: DropCause::UnexpectedFragment { index: 3, count: 3 }
            }
        );
        // A count of zero.
        assert!(matches!(
            buf.accept(0, peer, &piece(1, 0, 0, 100)),
            ReassemblyOutcome::Failed { .. }
        ));
        // A second fragment that disagrees about the count.
        buf.accept(0, peer, &piece(2, 0, 3, 100));
        assert_eq!(
            buf.accept(NS_PER_MS, peer, &piece(2, 1, 4, 100)),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(2),
                cause: DropCause::FragmentCountMismatch { had: 3, saw: 4 }
            }
        );
        assert_eq!(buf.open(), 0, "the contradictory set is dropped whole");
        // A count above what the buffer can track.
        assert!(matches!(
            buf.accept(0, peer, &piece(3, 0, REASSEMBLY_MAX_FRAGMENTS + 1, 10)),
            ReassemblyOutcome::Failed {
                cause: DropCause::TooManyFragments { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_full_buffer_refuses_the_new_sdu() {
        let mut buf = ReassemblyBuffer::new(2, Duration::from_millis(1_000));
        let peer = NodeId::new(2);
        buf.accept(0, peer, &piece(1, 0, 2, 100));
        buf.accept(0, peer, &piece(2, 0, 2, 100));
        assert_eq!(buf.open(), 2);
        assert_eq!(
            buf.accept(0, peer, &piece(3, 0, 2, 100)),
            ReassemblyOutcome::Failed {
                sdu: SduId::new(3),
                cause: DropCause::ReassemblyBufferFull { open: 2, max: 2 }
            }
        );
        // …but an already-open SDU still progresses.
        assert!(matches!(
            buf.accept(0, peer, &piece(1, 1, 2, 100)),
            ReassemblyOutcome::Complete { .. }
        ));
    }

    /// Two peers sending the same SDU id are two separate reassemblies.
    #[test]
    fn the_buffer_is_keyed_per_peer() {
        let mut buf = ReassemblyBuffer::new(4, Duration::from_millis(1_000));
        buf.accept(0, NodeId::new(1), &piece(5, 0, 2, 100));
        let out = buf.accept(0, NodeId::new(2), &piece(5, 1, 2, 100));
        assert!(
            matches!(out, ReassemblyOutcome::Pending { have: 1, .. }),
            "a fragment from another peer does not complete this one"
        );
        assert_eq!(buf.open(), 2);
    }

    #[test]
    fn the_record_reports_the_outcome_without_the_senders_identity() {
        let outcome = ReassemblyOutcome::Expired {
            sdu: SduId::new(4),
            have: 2,
            want: 5,
            bytes: 800,
        };
        let rec = FragRecord::new(1_234_000, NodeId::new(7), &outcome);
        assert_eq!(rec.outcome, "expired");
        assert_eq!((rec.fragments, rec.have, rec.bytes), (5, 2, 800));
        assert_eq!(FragRecord::CHANNEL, "net.frag");
        assert_eq!(FragRecord::VISIBILITY, Visibility::Node);
        assert!(
            Visibility::Node.allowed_on_node_channel(),
            "and a NODE record may be written to a NODE channel"
        );
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("from"), "no sender identity: {json}");
    }

    #[test]
    fn the_memory_of_a_completion_is_bounded_and_released_on_time() {
        // The completion memory must not grow without limit, and it must not outlive the
        // window in which a repeat is still a repeat. Both are asserted here rather than
        // argued, because the whole point of the memory is that it costs less than the
        // reassembly slot it replaces.
        let mut b = ReassemblyBuffer::new(2, Duration::from_millis(1_000));
        let peer = NodeId::new(1);
        let complete = |b: &mut ReassemblyBuffer, now: SimTime, sdu: u32| {
            b.accept(now, peer, &piece(sdu, 0, 2, 100));
            b.accept(now, peer, &piece(sdu, 1, 2, 100))
        };
        for sdu in 1..=2u32 {
            assert!(matches!(
                complete(&mut b, 0, sdu),
                ReassemblyOutcome::Complete { .. }
            ));
        }
        assert_eq!(b.open(), 0, "nothing is still being reassembled");
        assert_eq!(b.remembered(), 2);
        // A third completion evicts the oldest remembered key rather than growing.
        assert!(matches!(
            complete(&mut b, 10 * NS_PER_MS, 3),
            ReassemblyOutcome::Complete { .. }
        ));
        assert_eq!(b.remembered(), 2, "the memory grew past max_open");
        // SDU 1 was the oldest, so it is the one that went; SDU 2 and 3 are still known.
        assert!(matches!(
            b.accept(20 * NS_PER_MS, peer, &piece(1, 0, 2, 100)),
            ReassemblyOutcome::Pending {
                have: 1,
                want: 2,
                ..
            }
        ));
        for sdu in [2u32, 3] {
            assert!(
                matches!(
                    b.accept(20 * NS_PER_MS, peer, &piece(sdu, 0, 2, 100)),
                    ReassemblyOutcome::Duplicate { segments: 2, .. }
                ),
                "sdu {sdu} was forgotten early"
            );
        }
        // And one timeout after the completion it records, the memory goes — silently, with
        // no outcome queued, because the content was delivered.
        assert!(b.expire(1_010 * NS_PER_MS).is_empty());
        assert_eq!(b.remembered(), 0);
        assert!(matches!(
            b.accept(1_010 * NS_PER_MS, peer, &piece(3, 0, 2, 100)),
            ReassemblyOutcome::Pending {
                have: 1,
                want: 2,
                ..
            }
        ));
    }

    #[test]
    fn outcome_helpers_agree_with_the_variants() {
        let sdu = SduId::new(3);
        let pending = ReassemblyOutcome::Pending {
            sdu,
            have: 1,
            want: 2,
        };
        let failed = ReassemblyOutcome::Failed {
            sdu,
            cause: DropCause::Mtu {
                sdu_bytes: 10,
                mtu: 5,
            },
        };
        assert_eq!(pending.sdu(), sdu);
        assert!(!pending.is_loss());
        assert!(failed.is_loss());
        assert_eq!(failed.label(), "failed");
        assert!(FragmentKind::Segment.independently_interpretable());
        assert!(FragmentKind::Whole.independently_interpretable());
        assert!(!FragmentKind::Piece.independently_interpretable());
        assert!(!FragmentKind::CertFragment.independently_interpretable());
        assert!(FragmentDesc::whole(sdu, 10).is_whole());
        assert_eq!(FragmentDesc::whole(sdu, 10).total_bytes(), 10);
    }
}
