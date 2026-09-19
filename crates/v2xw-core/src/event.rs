//! The deterministic event scheduler.
//!
//! One binary heap holds every cross-entity event of a run, ordered by
//! `(time, priority, seq)` (02-architecture.md §5.1, ADR 0004 §1):
//!
//! * **time** — [`crate::time::SimTime`] nanoseconds since `t0`;
//! * **priority** — fixed per [`EventClass`], never chosen by a plug-in, so that what
//!   happens first at a shared instant is a documented rule rather than an accident;
//! * **seq** — a single global monotonic counter stamped at [`Scheduler::schedule`], so
//!   two events with equal time and priority run in the order they were scheduled.
//!
//! Because scheduling itself happens in a deterministic order (id-sorted merges,
//! 02-architecture.md §6.4), the whole run is deterministic.
//!
//! The scheduler is generic over the payload type `E`, so this crate stays free of
//! domain types: the kernel crate instantiates `Scheduler<EventPayload>` with its own
//! enum of radio, node and protocol events.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};

use crate::time::{Duration, SimTime};

/// The classes of event the kernel knows, with their fixed priorities.
///
/// The table is 02-architecture.md §5.1 verbatim. Adding a class means adding a row
/// there and a variant here; a plug-in cannot invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventClass {
    /// Priority 0 — scenario control: parameter change, outage, road closure, attack
    /// wave. Must be visible to everything else at the same instant.
    Control,
    /// Priority 1 — the periodic mobility step. Kinematics for time *t* are final
    /// before any radio event at *t* reads them.
    MobilityStep,
    /// Priority 2 — traffic-signal phase change: intersection state before vehicles
    /// decide.
    SignalPhase,
    /// Priority 3 — end of a frame's arrival, i.e. the reception outcome. Reception is
    /// decided before the MAC reacts.
    PhyEnd,
    /// Priority 4 — MAC timers: backoff, AIFS, SPS reservation.
    MacTimer,
    /// Priority 5 — a transmission begins, after the MAC decisions at the same instant.
    PhyStart,
    /// Priority 6 — node tasks: verification done, application timer, generator tick.
    NodeTask,
    /// Priority 7 — delivery on backhaul, cellular or backend links.
    NetDeliver,
    /// Priority 8 — protocol timers and batch windows.
    FlowTimer,
    /// Priority 9 — observation of a fully settled instant. This is the row the
    /// architecture table calls `Metric / Keyframe / Export`: metric sampling, UI
    /// keyframes and exporter flushes all share this class and priority, because they
    /// all want the same thing — to look at an instant after everything else at that
    /// instant has happened.
    Observe,
}

impl EventClass {
    /// The class's fixed priority, `0` (earliest) to `9` (latest).
    ///
    /// **Stability warning:** these numbers are part of the determinism contract; every
    /// golden digest depends on them.
    pub const fn priority(&self) -> u8 {
        match self {
            EventClass::Control => 0,
            EventClass::MobilityStep => 1,
            EventClass::SignalPhase => 2,
            EventClass::PhyEnd => 3,
            EventClass::MacTimer => 4,
            EventClass::PhyStart => 5,
            EventClass::NodeTask => 6,
            EventClass::NetDeliver => 7,
            EventClass::FlowTimer => 8,
            EventClass::Observe => 9,
        }
    }

    /// Every class, in priority order. Useful for reports and for exhaustiveness tests.
    pub const ALL: [EventClass; 10] = [
        EventClass::Control,
        EventClass::MobilityStep,
        EventClass::SignalPhase,
        EventClass::PhyEnd,
        EventClass::MacTimer,
        EventClass::PhyStart,
        EventClass::NodeTask,
        EventClass::NetDeliver,
        EventClass::FlowTimer,
        EventClass::Observe,
    ];
}

impl core::fmt::Display for EventClass {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The total order of events.
///
/// `Ord` is the **dispatch order**: ascending time, then ascending priority, then
/// ascending `seq`, so `a < b` means "`a` runs before `b`". Rust's [`BinaryHeap`] is a
/// *max*-heap, so [`Scheduler`] stores `Reverse<Entry>` rather than inverting this
/// comparison — the natural order is the useful one for assertions, logs and tests, and
/// the reversal lives in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventKey {
    /// When the event fires.
    pub time: SimTime,
    /// The priority of the event's class ([`EventClass::priority`]).
    pub priority: u8,
    /// Global monotonic sequence number, stamped at schedule time.
    pub seq: u64,
}

impl EventKey {
    /// Creates a key. Normally produced by [`Scheduler::schedule`], not by hand.
    pub const fn new(time: SimTime, priority: u8, seq: u64) -> Self {
        Self {
            time,
            priority,
            seq,
        }
    }
}

/// A handle to a scheduled event, used to cancel it.
///
/// It wraps the event's `seq`, which is unique within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventHandle(
    /// The `seq` of the scheduled event.
    pub u64,
);

impl EventHandle {
    /// The sequence number this handle refers to.
    pub const fn seq(self) -> u64 {
        self.0
    }
}

/// A heap entry: the key plus the payload. Ordered by key alone.
#[derive(Debug)]
struct Entry<E> {
    key: EventKey,
    payload: E,
}

impl<E> PartialEq for Entry<E> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl<E> Eq for Entry<E> {}
impl<E> PartialOrd for Entry<E> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<E> Ord for Entry<E> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// The event heap.
///
/// Generic over the payload `E`. Cancellation is lazy: [`Scheduler::cancel`] forgets the
/// handle and [`Scheduler::pop`] skips entries whose handle is no longer live, which
/// keeps `cancel` O(1) amortised and never reorders the heap. Because the engine's
/// cancel-heavy patterns (MAC backoff timers abandoned when the channel goes busy, SPS
/// reservations, protocol timeouts rescheduled every tick) would otherwise pin a dead
/// entry in the heap for every cancellation of a far-future event, the scheduler counts
/// them and compacts the heap once they outnumber the live ones
/// ([`Scheduler::pending_entries`]).
#[derive(Debug)]
pub struct Scheduler<E> {
    heap: BinaryHeap<Reverse<Entry<E>>>,
    /// Sequence numbers still scheduled: `pop` drops anything not in here.
    ///
    /// A `BTreeSet` rather than a `HashSet`: nothing iterates it today, and a
    /// `HashSet`'s iteration order is both unspecified and seeded per process, which is
    /// precisely the kind of thing that later leaks into an output (02-architecture.md
    /// §6.1). The ordered set costs a few nanoseconds per event and removes the question.
    live: BTreeSet<u64>,
    /// Entries in `heap` whose `seq` is no longer in `live`, i.e. reclaimable space.
    dead: usize,
    next_seq: u64,
    now: SimTime,
    /// The key of the last event [`Scheduler::pop`] returned, for the dispatch-order
    /// assertions.
    last_key: Option<EventKey>,
    /// Zero-delay, earlier-priority schedules deliberately requested through
    /// [`Scheduler::schedule_reentrant`] and not yet dispatched, which the debug-build
    /// order check must not flag.
    reentrant_debt: u64,
}

impl<E> Default for Scheduler<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Scheduler<E> {
    /// Creates an empty scheduler whose clock reads `0`.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            live: BTreeSet::new(),
            dead: 0,
            next_seq: 0,
            now: 0,
            last_key: None,
            reentrant_debt: 0,
        }
    }

    /// Schedules `payload` to fire at `at` with the priority of `class`.
    ///
    /// The returned [`EventHandle`] can cancel it. `seq` comes from a single global
    /// monotonic counter, so two events scheduled for the same time and priority fire in
    /// the order they were scheduled.
    ///
    /// # Panics
    /// If `at` is before [`Scheduler::now`]. Scheduling into the past would break the
    /// monotonicity the whole kernel relies on; a model that computes a past deadline
    /// has a bug, and silently clamping it would hide the bug in the results.
    ///
    /// Also if `(at, class.priority())` is *before the key being dispatched*: a handler
    /// running at instant `t` scheduling an earlier-priority event
    /// at the same `t`. That event would be dispatched next, so the sequence of
    /// [`EventKey`]s coming out of [`Scheduler::pop`] would not be non-decreasing, and the
    /// two guarantees the priority table exists to provide — `Control` "must be visible to
    /// everything else at the same instant", `Observe` "observes a fully settled instant"
    /// (02-architecture.md §5.1) — would be silently void. Anything that consumes the
    /// dispatch stream in key order (recorder, metrics, VWP keyframes) would then be
    /// wrong with no diagnostic. A handler that genuinely means it says so with
    /// [`Scheduler::schedule_reentrant`].
    pub fn schedule(&mut self, at: SimTime, class: EventClass, payload: E) -> EventHandle {
        self.assert_not_past(at, class);
        // A hard assertion, like the "not in the past" one above and for the same reason:
        // it costs one comparison, it catches the caller's bug at the point of the bug,
        // and a release build that let it through would silently reorder the run's output.
        assert!(
            self.last_key
                .is_none_or(|k| (at, class.priority()) >= (k.time, k.priority)),
            "zero-delay event at an earlier priority than the instant being dispatched: \
             ({at} ns, {class} = priority {}) while dispatching {:?}",
            class.priority(),
            self.last_key
        );
        self.push(at, class, payload)
    }

    /// Schedules `payload` to fire `delay` after the current instant — "now plus this
    /// much", which is what a timer, a backoff, a service time or a repetition interval
    /// actually is.
    ///
    /// Exactly [`Scheduler::schedule`] at `Scheduler::now() + delay`, with the addition
    /// done once and saturating ([`crate::time::Duration`]), so a model cannot produce a
    /// deadline in the past by arithmetic and cannot wrap `u64` in a release build. Before
    /// the first [`Scheduler::pop`] the clock reads `0`, so `schedule_after` on a fresh
    /// scheduler is the same as scheduling at `delay`.
    ///
    /// A zero delay is legal and means "later at this same instant", subject to the same
    /// priority rule as [`Scheduler::schedule`]: the class must not sit earlier in the total
    /// order than the event being dispatched.
    ///
    /// # Panics
    /// Under exactly the conditions [`Scheduler::schedule`] panics — it cannot be "in the
    /// past", but a zero-delay event at an earlier priority is still a caller bug.
    pub fn schedule_after(
        &mut self,
        delay: Duration,
        class: EventClass,
        payload: E,
    ) -> EventHandle {
        self.schedule(delay.after(self.now), class, payload)
    }

    /// Schedules an event that may sit *earlier* in the total order than the event being
    /// dispatched — a zero-delay, earlier-priority injection at the current instant.
    ///
    /// Named rather than silent, because it breaks the property every ordinary consumer
    /// may assume: that [`Scheduler::pop`] returns keys in non-decreasing order. A
    /// recorder, metric or keyframe writer that batches by key must handle a key that goes
    /// backwards, or the run's outputs will be ordered differently from its events. Use
    /// [`Scheduler::schedule`] unless that has been thought through.
    ///
    /// # Panics
    /// If `at` is before [`Scheduler::now`], exactly like [`Scheduler::schedule`]. Real
    /// time still never goes backwards; only the priority within an instant does.
    pub fn schedule_reentrant(
        &mut self,
        at: SimTime,
        class: EventClass,
        payload: E,
    ) -> EventHandle {
        self.assert_not_past(at, class);
        if self
            .last_key
            .is_some_and(|k| (at, class.priority()) < (k.time, k.priority))
        {
            self.reentrant_debt += 1;
        }
        self.push(at, class, payload)
    }

    /// The one hard rule both entry points share: simulated time never goes backwards.
    fn assert_not_past(&self, at: SimTime, class: EventClass) {
        assert!(
            at >= self.now,
            "event scheduled in the past: at {at} ns, now {} ns ({class})",
            self.now
        );
    }

    /// The scheduling both entry points share, after their order checks.
    fn push(&mut self, at: SimTime, class: EventClass, payload: E) -> EventHandle {
        let seq = self.next_seq;
        self.next_seq += 1;
        let key = EventKey::new(at, class.priority(), seq);
        self.heap.push(Reverse(Entry { key, payload }));
        self.live.insert(seq);
        EventHandle(seq)
    }

    /// Cancels a scheduled event.
    ///
    /// Returns `true` if the event was still pending, `false` if it had already fired,
    /// had already been cancelled, or never existed. The entry stays in the heap until
    /// [`Scheduler::pop`] reaches it, or until the accumulated dead entries trigger a
    /// compaction — without which a run that cancels a far-future timer on every tick
    /// would grow the heap for the whole run while [`Scheduler::len`] reported it as
    /// nearly empty.
    pub fn cancel(&mut self, h: EventHandle) -> bool {
        if !self.live.remove(&h.0) {
            return false;
        }
        self.dead += 1;
        self.compact_if_needed();
        true
    }

    /// Rebuilds the heap without its dead entries once they outnumber the live ones.
    ///
    /// The threshold keeps the amortised cost of a cancellation O(1): each rebuild is
    /// O(n) and halves the heap, so a run pays at most a constant per cancelled event.
    /// Rebuilding cannot change the dispatch order — [`EventKey`] is a strict total order
    /// (`seq` is unique), so the surviving entries come out in the same sequence whatever
    /// the heap's internal layout.
    fn compact_if_needed(&mut self) {
        if self.dead <= 64 || self.dead <= self.heap.len() / 2 {
            return;
        }
        let live = &self.live;
        let kept: Vec<Reverse<Entry<E>>> = core::mem::take(&mut self.heap)
            .into_vec()
            .into_iter()
            .filter(|Reverse(e)| live.contains(&e.key.seq))
            .collect();
        self.heap = BinaryHeap::from(kept);
        self.dead = 0;
    }

    /// Removes and returns the next event, advancing the clock to its time.
    ///
    /// Returns `None` when nothing is left. Cancelled entries are discarded on the way.
    ///
    /// # Panics
    /// In debug builds, if the popped key is lower than the last one dispatched — heap
    /// corruption, or a zero-delay earlier-priority schedule that did not go through
    /// [`Scheduler::schedule_reentrant`].
    pub fn pop(&mut self) -> Option<(EventKey, E)> {
        while let Some(Reverse(entry)) = self.heap.pop() {
            if self.live.remove(&entry.key.seq) {
                self.check_dispatch_order(entry.key);
                self.now = entry.key.time;
                self.last_key = Some(entry.key);
                return Some((entry.key, entry.payload));
            }
            self.dead -= 1;
        }
        None
    }

    /// Debug-build check that the dispatch sequence is non-decreasing in [`EventKey`].
    fn check_dispatch_order(&mut self, key: EventKey) {
        debug_assert!(
            key.time >= self.now,
            "scheduler time went backwards: {} ns after {} ns",
            key.time,
            self.now
        );
        if !cfg!(debug_assertions) {
            return;
        }
        match self.last_key {
            Some(last) if key < last => {
                assert!(
                    self.reentrant_debt > 0,
                    "dispatch order went backwards: {key:?} after {last:?}"
                );
                self.reentrant_debt -= 1;
            }
            _ => {}
        }
    }

    /// The time of the next event, or `None` if the heap is empty.
    ///
    /// Takes `&mut self` because it first discards cancelled entries at the head of the
    /// heap; without that, a cancelled event could report a time that never arrives.
    pub fn peek_time(&mut self) -> Option<SimTime> {
        self.purge_head();
        self.heap.peek().map(|Reverse(e)| e.key.time)
    }

    /// The key of the next event, or `None` if the heap is empty. Discards cancelled
    /// entries at the head, like [`Scheduler::peek_time`].
    pub fn peek_key(&mut self) -> Option<EventKey> {
        self.purge_head();
        self.heap.peek().map(|Reverse(e)| e.key)
    }

    /// The time of the last popped event: the kernel's current simulated instant.
    ///
    /// Monotonically non-decreasing, `0` before the first [`Scheduler::pop`].
    pub fn now(&self) -> SimTime {
        self.now
    }

    /// Number of scheduled events not yet fired or cancelled.
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// True if no event is pending.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Entries physically in the heap, live and cancelled-but-not-yet-reclaimed.
    ///
    /// Always at least [`Scheduler::len`]. Exposed so a long run can be watched for heap
    /// growth and so the compaction rule has a testable observable; the kernel itself
    /// never branches on it.
    pub fn pending_entries(&self) -> usize {
        self.heap.len()
    }

    /// The key of the last event [`Scheduler::pop`] returned, or `None` before the first.
    pub fn last_key(&self) -> Option<EventKey> {
        self.last_key
    }

    /// The `seq` the next [`Scheduler::schedule`] will use. Exposed for snapshots.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Drops every pending event, keeping the clock and the sequence counter — so
    /// handles stay unique across a clear.
    pub fn clear(&mut self) {
        self.heap.clear();
        self.live.clear();
        self.dead = 0;
    }

    /// Discards cancelled entries sitting at the head of the heap.
    fn purge_head(&mut self) {
        while let Some(Reverse(entry)) = self.heap.peek() {
            if self.live.contains(&entry.key.seq) {
                return;
            }
            self.heap.pop();
            self.dead -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{NS_PER_MS, NS_PER_S};

    #[test]
    fn priorities_match_the_architecture_table() {
        assert_eq!(EventClass::Control.priority(), 0);
        assert_eq!(EventClass::MobilityStep.priority(), 1);
        assert_eq!(EventClass::SignalPhase.priority(), 2);
        assert_eq!(EventClass::PhyEnd.priority(), 3);
        assert_eq!(EventClass::MacTimer.priority(), 4);
        assert_eq!(EventClass::PhyStart.priority(), 5);
        assert_eq!(EventClass::NodeTask.priority(), 6);
        assert_eq!(EventClass::NetDeliver.priority(), 7);
        assert_eq!(EventClass::FlowTimer.priority(), 8);
        assert_eq!(EventClass::Observe.priority(), 9);
        // The list is complete and in order, with no duplicate priorities.
        for (i, c) in EventClass::ALL.iter().enumerate() {
            assert_eq!(c.priority() as usize, i);
        }
    }

    #[test]
    fn event_key_orders_ascending_for_dispatch() {
        let a = EventKey::new(10, 1, 5);
        let b = EventKey::new(10, 1, 6);
        let c = EventKey::new(10, 2, 0);
        let d = EventKey::new(11, 0, 0);
        assert!(a < b, "equal time and priority: lower seq first");
        assert!(b < c, "equal time: lower priority first");
        assert!(c < d, "lower time first");
        // And Reverse() flips it, which is what the max-heap needs.
        assert!(Reverse(d) < Reverse(a));
    }

    #[test]
    fn pops_in_time_priority_insertion_order() {
        let mut s: Scheduler<&str> = Scheduler::new();
        // Deliberately scheduled out of order.
        s.schedule(2 * NS_PER_S, EventClass::Control, "late-control");
        s.schedule(NS_PER_S, EventClass::Observe, "observe");
        s.schedule(NS_PER_S, EventClass::Control, "control");
        s.schedule(NS_PER_S, EventClass::PhyEnd, "phy-end-first");
        s.schedule(NS_PER_S, EventClass::PhyEnd, "phy-end-second");
        s.schedule(NS_PER_S, EventClass::MobilityStep, "mobility");

        let order: Vec<&str> = std::iter::from_fn(|| s.pop().map(|(_, p)| p)).collect();
        assert_eq!(
            order,
            vec![
                "control",
                "mobility",
                "phy-end-first",
                "phy-end-second",
                "observe",
                "late-control",
            ]
        );
    }

    /// A delay is measured from the instant being dispatched, not from `t0`, and the
    /// arithmetic saturates rather than wrapping.
    #[test]
    fn schedule_after_is_relative_to_the_current_instant() {
        use crate::time::Duration;

        let mut s: Scheduler<&str> = Scheduler::new();
        // Before the first pop the clock reads 0, so a delay is an absolute time.
        s.schedule_after(Duration::from_secs(1), EventClass::NodeTask, "first");
        let (k, p) = s.pop().unwrap();
        assert_eq!(p, "first");
        assert_eq!(k.time, NS_PER_S);
        assert_eq!(s.now(), NS_PER_S);

        // From here a delay is relative to `now`, which is the whole point.
        let h = s.schedule_after(
            Duration::from_millis(100),
            EventClass::NodeTask,
            "in 100 ms",
        );
        s.schedule_after(Duration::ZERO, EventClass::Observe, "later at this instant");
        s.schedule(3 * NS_PER_S, EventClass::NodeTask, "absolute");
        assert_eq!(
            s.peek_key().unwrap().time,
            NS_PER_S,
            "the zero delay is now"
        );

        let (k, p) = s.pop().unwrap();
        assert_eq!((k.time, p), (NS_PER_S, "later at this instant"));
        let (k, p) = s.pop().unwrap();
        assert_eq!((k.time, p), (NS_PER_S + 100 * NS_PER_MS, "in 100 ms"));
        assert!(!s.cancel(h), "the handle is the one that already fired");

        // The two spellings agree exactly.
        let mut a: Scheduler<u8> = Scheduler::new();
        let mut b: Scheduler<u8> = Scheduler::new();
        a.schedule(500 * NS_PER_MS, EventClass::MacTimer, 1);
        b.schedule_after(Duration::from_millis(500), EventClass::MacTimer, 1);
        assert_eq!(a.pop().unwrap().0, b.pop().unwrap().0);

        // A nonsense delay clamps to the end of time instead of wrapping into the past.
        let mut s: Scheduler<&str> = Scheduler::new();
        s.schedule(10 * NS_PER_S, EventClass::NodeTask, "anchor");
        s.pop().unwrap();
        s.schedule_after(Duration::MAX, EventClass::NodeTask, "never");
        assert_eq!(s.peek_key().unwrap().time, u64::MAX);
    }

    #[test]
    fn equal_time_and_priority_follow_insertion_order() {
        let mut s: Scheduler<u32> = Scheduler::new();
        for i in 0..100 {
            s.schedule(5 * NS_PER_MS, EventClass::NodeTask, i);
        }
        let order: Vec<u32> = std::iter::from_fn(|| s.pop().map(|(_, p)| p)).collect();
        assert_eq!(order, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn cancel_removes_exactly_one_event() {
        let mut s: Scheduler<&str> = Scheduler::new();
        let a = s.schedule(NS_PER_S, EventClass::NodeTask, "a");
        let b = s.schedule(NS_PER_S, EventClass::NodeTask, "b");
        let c = s.schedule(2 * NS_PER_S, EventClass::NodeTask, "c");
        assert_eq!(s.len(), 3);

        assert!(s.cancel(b));
        assert_eq!(s.len(), 2);
        assert!(!s.cancel(b), "cancelling twice is a no-op");
        assert!(!s.cancel(EventHandle(999)), "unknown handle");

        assert_eq!(s.pop().unwrap().1, "a");
        assert!(!s.cancel(a), "a has already fired");
        assert_eq!(s.pop().unwrap().1, "c");
        assert!(s.pop().is_none());
        assert!(!s.cancel(c));
        assert!(s.is_empty());
    }

    #[test]
    fn cancelled_head_is_not_reported_by_peek() {
        let mut s: Scheduler<&str> = Scheduler::new();
        let early = s.schedule(NS_PER_MS, EventClass::Control, "early");
        s.schedule(NS_PER_S, EventClass::Control, "late");
        assert_eq!(s.peek_time(), Some(NS_PER_MS));
        s.cancel(early);
        assert_eq!(s.peek_time(), Some(NS_PER_S));
        assert_eq!(s.peek_key().unwrap().time, NS_PER_S);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn time_is_monotonically_non_decreasing() {
        let mut s: Scheduler<u64> = Scheduler::new();
        let mut t = 0;
        for i in 0..200u64 {
            // Pseudo-random but fixed schedule times.
            t = (t + (i * 37) % 11 * NS_PER_MS) % (NS_PER_S * 3);
            s.schedule(t, EventClass::ALL[(i % 10) as usize], i);
        }
        assert_eq!(s.now(), 0);
        let mut last = 0;
        while let Some((key, _)) = s.pop() {
            assert!(key.time >= last);
            assert_eq!(s.now(), key.time);
            last = key.time;
        }
        assert_eq!(s.now(), last);
    }

    #[test]
    #[should_panic(expected = "event scheduled in the past")]
    fn scheduling_in_the_past_panics() {
        let mut s: Scheduler<()> = Scheduler::new();
        s.schedule(NS_PER_S, EventClass::Control, ());
        let _ = s.pop();
        s.schedule(NS_PER_MS, EventClass::Control, ());
    }

    #[test]
    fn scheduling_at_the_current_instant_is_allowed() {
        let mut s: Scheduler<&str> = Scheduler::new();
        s.schedule(NS_PER_S, EventClass::MacTimer, "first");
        let (k, _) = s.pop().unwrap();
        // A MAC timer handler reacting at the same instant with a later class.
        s.schedule(k.time, EventClass::PhyStart, "same-instant");
        // The same class again at the same instant is fine too: equal keys, later seq.
        s.schedule(k.time, EventClass::MacTimer, "same-class");
        assert_eq!(s.pop().unwrap().1, "same-class");
        assert_eq!(s.pop().unwrap().1, "same-instant");
        assert_eq!(
            s.last_key().unwrap().priority,
            EventClass::PhyStart.priority()
        );
    }

    /// The dispatch sequence must be non-decreasing in [`EventKey`], or the priority
    /// table's guarantees ("`Control` is visible to everything else at the same instant",
    /// "`Observe` sees a settled instant") mean nothing and every consumer that assumes
    /// key order is silently wrong.
    #[test]
    #[should_panic(expected = "earlier priority than the instant being dispatched")]
    fn zero_delay_back_priority_scheduling_is_rejected() {
        let mut s: Scheduler<&str> = Scheduler::new();
        s.schedule(NS_PER_S, EventClass::PhyEnd, "phy-end");
        let (k, _) = s.pop().unwrap();
        // A PhyEnd handler injecting a Control event at the very same instant: it would be
        // dispatched next, with a key strictly lower than the one just dispatched.
        s.schedule(k.time, EventClass::Control, "too-late-control");
    }

    /// …unless the handler says it means it, in which case the order check steps aside
    /// for exactly that event.
    #[test]
    fn schedule_reentrant_names_the_exception() {
        let mut s: Scheduler<&str> = Scheduler::new();
        s.schedule(NS_PER_S, EventClass::PhyEnd, "phy-end");
        let (k, _) = s.pop().unwrap();
        s.schedule_reentrant(k.time, EventClass::Control, "deliberate");
        s.schedule(k.time, EventClass::NodeTask, "ordinary");
        let (back, p) = s.pop().unwrap();
        assert_eq!(p, "deliberate");
        assert!(back < k, "the reentrant event really is out of order");
        assert_eq!(s.pop().unwrap().1, "ordinary");
        // A later instant is unaffected, and the ordinary path is guarded again.
        s.schedule(2 * NS_PER_S, EventClass::Control, "next-instant");
        assert_eq!(s.pop().unwrap().1, "next-instant");
    }

    /// Cancelling far-future events must reclaim heap space. A 24-hour backend run that
    /// reschedules one timer per node per tick (02-architecture.md §5.4) would otherwise
    /// pin ~10⁹ dead entries that `len()` reports as absent.
    #[test]
    fn cancelling_reclaims_heap_space() {
        let mut s: Scheduler<u32> = Scheduler::new();
        let mut handles = Vec::with_capacity(100_000);
        for i in 0..100_000u32 {
            // All far in the future, so nothing ever reaches the head of the heap.
            handles.push(s.schedule(NS_PER_S + i as SimTime, EventClass::MacTimer, i));
        }
        assert_eq!(s.pending_entries(), 100_000);
        for h in handles.iter().take(99_000) {
            assert!(s.cancel(*h));
        }
        assert_eq!(s.len(), 1_000, "live count is the small one");
        assert!(
            s.pending_entries() < 4_000,
            "heap still holds {} entries for {} live events",
            s.pending_entries(),
            s.len()
        );

        // Compaction preserves the dispatch order exactly.
        let popped: Vec<u32> = std::iter::from_fn(|| s.pop().map(|(_, p)| p)).collect();
        assert_eq!(popped, (99_000..100_000).collect::<Vec<_>>());
        assert_eq!(s.pending_entries(), 0);
    }

    /// The reclamation is amortised, so a steady cancel-and-reschedule loop keeps the heap
    /// proportional to the live set rather than to the run's length.
    #[test]
    fn steady_cancel_and_reschedule_does_not_grow_the_heap() {
        let mut s: Scheduler<u64> = Scheduler::new();
        let mut pending: Vec<EventHandle> = (0..500)
            .map(|i| s.schedule(10 * NS_PER_S, EventClass::FlowTimer, i))
            .collect();
        for round in 0..200u64 {
            for h in core::mem::take(&mut pending) {
                s.cancel(h);
            }
            pending = (0..500)
                .map(|i| s.schedule(10 * NS_PER_S, EventClass::FlowTimer, round * 1000 + i))
                .collect();
        }
        assert_eq!(s.len(), 500);
        assert!(
            s.pending_entries() < 5_000,
            "heap grew to {} entries for 500 live events",
            s.pending_entries()
        );
    }

    /// Golden test: a fixed schedule produces a fixed pop order and fixed keys.
    #[test]
    fn golden_pop_order() {
        let mut s: Scheduler<u32> = Scheduler::new();
        let script: [(SimTime, EventClass); 12] = [
            (100, EventClass::NodeTask),
            (100, EventClass::Control),
            (50, EventClass::Observe),
            (50, EventClass::PhyStart),
            (100, EventClass::Control),
            (0, EventClass::MobilityStep),
            (200, EventClass::FlowTimer),
            (50, EventClass::PhyStart),
            (0, EventClass::Control),
            (200, EventClass::NetDeliver),
            (100, EventClass::PhyEnd),
            (50, EventClass::MacTimer),
        ];
        for (i, (at, class)) in script.iter().enumerate() {
            s.schedule(*at, *class, i as u32);
        }
        let popped: Vec<(SimTime, u8, u64, u32)> =
            std::iter::from_fn(|| s.pop().map(|(k, p)| (k.time, k.priority, k.seq, p))).collect();
        assert_eq!(
            popped,
            vec![
                (0, 0, 8, 8),
                (0, 1, 5, 5),
                (50, 4, 11, 11),
                (50, 5, 3, 3),
                (50, 5, 7, 7),
                (50, 9, 2, 2),
                (100, 0, 1, 1),
                (100, 0, 4, 4),
                (100, 3, 10, 10),
                (100, 6, 0, 0),
                (200, 7, 9, 9),
                (200, 8, 6, 6),
            ]
        );
    }

    #[test]
    fn clear_keeps_handles_unique() {
        let mut s: Scheduler<u8> = Scheduler::new();
        let a = s.schedule(NS_PER_S, EventClass::Control, 1);
        s.clear();
        assert!(s.is_empty());
        let b = s.schedule(NS_PER_S, EventClass::Control, 2);
        assert_ne!(a, b);
        assert_eq!(s.next_seq(), 2);
        assert!(!s.cancel(a));
        assert!(s.cancel(b));
    }

    #[test]
    fn serde_round_trips_keys_and_classes() {
        let k = EventKey::new(1_234, 3, 9);
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(serde_json::from_str::<EventKey>(&s).unwrap(), k);
        assert_eq!(
            serde_json::to_string(&EventClass::MobilityStep).unwrap(),
            "\"mobility-step\""
        );
        assert_eq!(serde_json::to_string(&EventHandle(7)).unwrap(), "7");
        assert_eq!(EventClass::PhyEnd.to_string(), "PhyEnd");
    }
}
