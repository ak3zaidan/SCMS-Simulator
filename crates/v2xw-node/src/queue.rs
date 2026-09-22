//! Bounded queues and their drop accounting (06-node-models.md §2.1, §2.4).
//!
//! Every queue in a node is finite, and the interesting behaviour of an overloaded node is
//! entirely in *which* queue overflowed and *why*. The telemetry record has six separate
//! drop counters for that reason (§3.5.2 offsets 60 to 80), so a queue here never simply
//! "drops": it drops with a [`DropCause`] that lands in its own counter.

use std::collections::VecDeque;

use v2xw_core::math::quantile_sorted;
use v2xw_core::time::SimTime;

/// Why a node discarded something. One variant per counter in the telemetry record
/// (vwp-v1 §3.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DropCause {
    /// The receive queue was full when a frame arrived.
    RxOverflow,
    /// The verification policy chose not to verify this message.
    VerifyPolicySkip,
    /// The verify queue was full.
    VerifyOverflow,
    /// The transmit queue was full.
    TxOverflow,
    /// A fragmented SDU never completed.
    ReassemblyTimeout,
    /// CRL processing fell so far behind that work was shed.
    CrlBacklog,
}

impl DropCause {
    /// Every cause, in the order the telemetry record lists them.
    pub const ALL: [DropCause; 6] = [
        DropCause::RxOverflow,
        DropCause::VerifyPolicySkip,
        DropCause::VerifyOverflow,
        DropCause::TxOverflow,
        DropCause::ReassemblyTimeout,
        DropCause::CrlBacklog,
    ];

    /// The spelling used in records (06-node-models.md §2.4).
    pub const fn as_str(self) -> &'static str {
        match self {
            DropCause::RxOverflow => "rx_overflow",
            DropCause::VerifyPolicySkip => "verify_policy_skip",
            DropCause::VerifyOverflow => "verify_overflow",
            DropCause::TxOverflow => "tx_overflow",
            DropCause::ReassemblyTimeout => "reassembly_timeout",
            DropCause::CrlBacklog => "crl_processing_backlog",
        }
    }
}

/// Which queue a task sits in (03-interfaces.md §8: `Task{queue: Rx|Verify|App|Tx|Crl}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueKind {
    /// Frames delivered by the PHY, awaiting parse.
    Rx,
    /// Parsed messages awaiting a signature check.
    Verify,
    /// Verified messages awaiting the applications.
    App,
    /// Frames awaiting the MAC.
    Tx,
    /// CRL expansion and sweep work.
    Crl,
}

impl QueueKind {
    /// Every queue, in the order the telemetry record lists their depths.
    pub const ALL: [QueueKind; 5] = [
        QueueKind::Rx,
        QueueKind::Verify,
        QueueKind::App,
        QueueKind::Tx,
        QueueKind::Crl,
    ];

    /// The drop cause an overflow of this queue carries.
    ///
    /// `App` and `Crl` are the two with no counter of their own: an application backlog
    /// sheds into [`DropCause::CrlBacklog`] only for CRL work, and an overfull app queue
    /// is reported through the depth percentiles rather than as a drop, because
    /// §3.5.2 has no `drop_app_overflow` field and inventing one would put a number on the
    /// wire that no reader knows how to interpret.
    pub const fn overflow_cause(self) -> Option<DropCause> {
        match self {
            QueueKind::Rx => Some(DropCause::RxOverflow),
            QueueKind::Verify => Some(DropCause::VerifyOverflow),
            QueueKind::Tx => Some(DropCause::TxOverflow),
            QueueKind::Crl => Some(DropCause::CrlBacklog),
            QueueKind::App => None,
        }
    }
}

/// What happened to something offered to a full queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission<T> {
    /// It was queued.
    Queued,
    /// The queue was full and this item was refused.
    Refused(T),
    /// The queue was full and the oldest item was evicted to make room.
    Evicted(T),
}

/// A bounded FIFO with depth sampling and drop accounting.
///
/// Depth samples are taken at every enqueue and every dequeue, which is what makes the
/// p50/p95 the telemetry record carries a percentile *of the queue's own history* rather
/// than of the sampling instants the engine happened to choose.
#[derive(Debug, Clone)]
pub struct NodeQueue<T> {
    kind: QueueKind,
    capacity: usize,
    items: VecDeque<T>,
    drops: u32,
    depth_samples: Vec<u32>,
    high_water: usize,
}

impl<T> NodeQueue<T> {
    /// A queue of `capacity` items.
    pub fn new(kind: QueueKind, capacity: usize) -> Self {
        NodeQueue {
            kind,
            capacity,
            items: VecDeque::new(),
            drops: 0,
            depth_samples: Vec::new(),
            high_water: 0,
        }
    }

    /// Which queue this is.
    pub fn kind(&self) -> QueueKind {
        self.kind
    }

    /// How many items it holds.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The configured capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The deepest this queue has been since the last window reset.
    pub fn high_water(&self) -> usize {
        self.high_water
    }

    /// How many items this queue has refused or evicted since the last window reset.
    pub fn drops(&self) -> u32 {
        self.drops
    }

    /// Offers an item, refusing it when the queue is full.
    pub fn push(&mut self, item: T) -> Admission<T> {
        if self.items.len() >= self.capacity {
            self.drops = self.drops.saturating_add(1);
            return Admission::Refused(item);
        }
        self.items.push_back(item);
        self.sample();
        Admission::Queued
    }

    /// Offers an item, evicting the oldest when the queue is full.
    ///
    /// The `prioritized` verification policy drops by age when its queue exceeds the
    /// profile's `queue_depth` (06-node-models.md §2.1), which is this, not
    /// [`NodeQueue::push`]: a fresh safety message is worth more than a stale one, so the
    /// newcomer wins.
    pub fn push_evicting(&mut self, item: T) -> Admission<T> {
        if self.items.len() >= self.capacity {
            let evicted = self.items.pop_front();
            self.drops = self.drops.saturating_add(1);
            self.items.push_back(item);
            self.sample();
            return match evicted {
                Some(old) => Admission::Evicted(old),
                // Unreachable for a capacity above zero; a zero-capacity queue refuses
                // everything, which is the only sensible reading of "no room at all".
                None => Admission::Refused(self.items.pop_back().expect("just pushed")),
            };
        }
        self.items.push_back(item);
        self.sample();
        Admission::Queued
    }

    /// Takes the oldest item.
    pub fn pop(&mut self) -> Option<T> {
        let item = self.items.pop_front();
        if item.is_some() {
            self.sample();
        }
        item
    }

    /// The items, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Removes and returns every item, oldest first.
    pub fn drain(&mut self) -> Vec<T> {
        let out: Vec<T> = self.items.drain(..).collect();
        if !out.is_empty() {
            self.sample();
        }
        out
    }

    /// The depth percentiles over the samples taken since the last window reset.
    ///
    /// Returns `(p50, p95)` saturated at `u16::MAX`, which is what §3.5.2's saturation
    /// rule asks for: "any `u16` counter at its maximum means >= 65535".
    pub fn depth_percentiles(&self) -> (u16, u16) {
        if self.depth_samples.is_empty() {
            return (0, 0);
        }
        let mut sorted: Vec<f64> = self.depth_samples.iter().map(|&d| f64::from(d)).collect();
        // Depths are small non-negative integers, so a total order and a numeric sort
        // agree; `sort_total_order` is used anyway so no NaN can ever reach the quantile.
        v2xw_core::math::sort_total_order(&mut sorted);
        let p50 = quantile_sorted(&sorted, 0.50);
        let p95 = quantile_sorted(&sorted, 0.95);
        (saturate_u16(p50), saturate_u16(p95))
    }

    /// Clears the window counters, keeping the queued items.
    pub fn reset_window(&mut self) {
        self.drops = 0;
        self.depth_samples.clear();
        self.high_water = self.items.len();
    }

    fn sample(&mut self) {
        let d = self.items.len();
        self.high_water = self.high_water.max(d);
        // A node under sustained overload would otherwise accumulate one sample per
        // message for the whole window. The cap is a memory bound, not a statistical
        // choice, and it is large enough that a 1 s window at 10 Hz generation and 100
        // neighbours never reaches it.
        const MAX_SAMPLES: usize = 4096;
        if self.depth_samples.len() < MAX_SAMPLES {
            self.depth_samples.push(d as u32);
        }
    }
}

fn saturate_u16(v: f64) -> u16 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else if v >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        // The percentile of integer depths is interpolated, so rounding rather than
        // truncating keeps `p50` of a queue that is always at depth 1 equal to 1.
        v.round() as u16
    }
}

/// A per-cause drop ledger, kept per node and reset per telemetry window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DropLedger {
    counts: [u32; 6],
}

impl DropLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        DropLedger::default()
    }

    /// Records one drop.
    pub fn record(&mut self, cause: DropCause) {
        let i = DropCause::ALL.iter().position(|c| *c == cause).unwrap_or(0);
        self.counts[i] = self.counts[i].saturating_add(1);
    }

    /// Adds `n` drops of one cause.
    pub fn record_n(&mut self, cause: DropCause, n: u32) {
        let i = DropCause::ALL.iter().position(|c| *c == cause).unwrap_or(0);
        self.counts[i] = self.counts[i].saturating_add(n);
    }

    /// How many drops of this cause.
    pub fn count(&self, cause: DropCause) -> u32 {
        let i = DropCause::ALL.iter().position(|c| *c == cause).unwrap_or(0);
        self.counts[i]
    }

    /// Every count, in [`DropCause::ALL`] order.
    pub fn counts(&self) -> [u32; 6] {
        self.counts
    }

    /// Clears the ledger for the next window.
    pub fn reset(&mut self) {
        self.counts = [0; 6];
    }
}

/// A task waiting for a server, with the instant it was enqueued at.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued<T> {
    /// The work itself.
    pub item: T,
    /// When it entered the queue, on the node's own clock.
    pub enqueued_at: SimTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full queue refuses, counts the refusal, and hands the item back so the caller can
    /// attribute the drop rather than losing it silently.
    #[test]
    fn a_full_queue_refuses_and_counts() {
        let mut q = NodeQueue::new(QueueKind::Rx, 2);
        assert_eq!(q.push(1), Admission::Queued);
        assert_eq!(q.push(2), Admission::Queued);
        assert_eq!(q.push(3), Admission::Refused(3));
        assert_eq!(q.drops(), 1);
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(1));
    }

    /// The prioritised policy's oldest-drop: the newcomer is admitted and the stale
    /// message is the one returned.
    #[test]
    fn oldest_drop_evicts_the_stale_message() {
        let mut q = NodeQueue::new(QueueKind::Verify, 2);
        q.push(1);
        q.push(2);
        assert_eq!(q.push_evicting(3), Admission::Evicted(1));
        assert_eq!(q.drops(), 1);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![2, 3]);
    }

    /// Percentiles come from the queue's own history, not from whenever the engine
    /// happened to look. A queue that oscillated between 10 and 11 for most of a window
    /// and then climbed to 40 reports a p50 in the steady band and a p95 well above it,
    /// which is the distinction a single instantaneous sample cannot make.
    ///
    /// The exact values are pinned rather than bracketed so that the interpolation rule
    /// is part of the contract: 440 samples, 201 of them at depth 11, put p50 at 11, and
    /// the 30-sample climb is 6.8 % of the window so p95 lands inside it at 18. The peak
    /// itself is the high-water mark, which is a different question and has its own
    /// accessor.
    #[test]
    fn depth_percentiles_follow_the_queues_history() {
        let mut q = NodeQueue::new(QueueKind::Verify, 64);
        for i in 0..10 {
            q.push(i);
        }
        // Sit at ten for a long stretch.
        for i in 0..200 {
            q.push(i);
            q.pop();
        }
        // One burst to forty.
        for i in 0..30 {
            q.push(i);
        }
        let (p50, p95) = q.depth_percentiles();
        assert_eq!(p50, 11, "p50 sits in the steady band");
        assert_eq!(p95, 18, "p95 sits inside the climb, above the steady band");
        assert!(p95 > p50);
        assert_eq!(q.high_water(), 40, "the peak is reported separately");
    }

    /// An empty window reports zero rather than a percentile of nothing.
    #[test]
    fn an_untouched_queue_reports_zero_depth() {
        let q: NodeQueue<u8> = NodeQueue::new(QueueKind::Tx, 8);
        assert_eq!(q.depth_percentiles(), (0, 0));
    }

    /// Each cause has its own counter, and the ledger saturates rather than wrapping.
    #[test]
    fn the_drop_ledger_keeps_causes_apart() {
        let mut l = DropLedger::new();
        l.record(DropCause::RxOverflow);
        l.record_n(DropCause::TxOverflow, 5);
        assert_eq!(l.count(DropCause::RxOverflow), 1);
        assert_eq!(l.count(DropCause::TxOverflow), 5);
        assert_eq!(l.count(DropCause::VerifyOverflow), 0);
        l.record_n(DropCause::RxOverflow, u32::MAX);
        assert_eq!(l.count(DropCause::RxOverflow), u32::MAX);
        l.reset();
        assert_eq!(l.counts(), [0; 6]);
    }
}
