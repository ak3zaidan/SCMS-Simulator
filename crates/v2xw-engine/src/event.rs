//! The concrete event payload, which instantiates [`v2xw_core::event::Scheduler`].
//!
//! Build decision D8 puts this here and nowhere else: `v2xw-core` owns the *kernel*
//! ([`EventClass`], [`EventKey`], the heap and the total order) and deliberately does not
//! own the payload, because a payload naming a radio frame and a node task would drag
//! every domain crate into the contract crate. Every model crate below this one therefore
//! reaches the scheduler through the narrowed per-family context traits of D12.2, which is
//! why none of them can name [`Event`] and none of them needs to.
//!
//! # The event table
//!
//! One row per [`EventClass`], in the fixed priority order of 02-architecture.md §5.1.
//! **Adding a class means adding a row here, a row there, and a variant in
//! [`EventClass`]** — `every_class_has_a_payload_variant` in this module's tests fails
//! otherwise, so the table cannot silently fall behind the enum.
//!
//! | Priority | Class | Payload variant | What it carries | Scheduled by |
//! |---|---|---|---|---|
//! | 0 | `Control` | [`Event::Control`] | the index of a scenario timeline item, and whether this is its start or its end | the timeline, at load |
//! | 1 | `MobilityStep` | [`Event::MobilityStep`] | nothing; the period is `time.mobility_step_ms` | itself, each step |
//! | 2 | `SignalPhase` | [`Event::SignalPhase`] | the signal whose controller advances | the mobility phase |
//! | 3 | `PhyEnd` | [`Event::PhyEnd`] | the frame whose arrival ends; its receiver set is resolved inside the phase | the PHY, at `PhyStart` |
//! | 4 | `MacTimer` | [`Event::MacTimer`] | the node and channel whose backoff, AIFS or SPS reservation expires | the MAC |
//! | 5 | `PhyStart` | [`Event::PhyStart`] | the frame whose transmission begins | the MAC, on a grant |
//! | 6 | `NodeTask` | [`Event::NodePhase`], [`Event::NodeTask`] | the batched step over every node, and one node's own wakeup | the mobility phase; the node phase |
//! | 7 | `NetDeliver` | [`Event::NetDeliver`] | the SDU and the node it reaches | the network layer |
//! | 8 | `FlowTimer` | [`Event::FlowTimer`] | the protocol flow and step whose timer expires | credential and backend protocols |
//! | 9 | `Observe` | [`Event::Observe`] | which observer: metric flush, UI keyframe, exporter flush | the run, periodically |
//!
//! # Why the variants are not one flat struct
//!
//! [`Event::class`] is total and `const`, so the mapping from payload to priority is a
//! match the compiler checks rather than a field a caller can set wrong. A payload that
//! carried its own class would let a node task be scheduled at `Control` priority, which
//! is exactly the ordering violation the kernel's dispatch assertion exists to catch —
//! and it would catch it one instant *after* the mistake was made.

use serde::{Deserialize, Serialize};
use v2xw_core::event::EventClass;
use v2xw_core::ids::{FrameSeq, NodeId, SduId, SignalId};

/// The kernel's event payload.
///
/// `Scheduler<Event>` is the engine's one heap. The heap holds only **cross-node** events
/// (ADR 0004 decision 5): a node's own queues are drained inside the phase-parallel node
/// map, so the global heap grows with the traffic between nodes rather than with the work
/// inside them. Appendix A's spike is the measurement that rule comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Event {
    /// Priority 0 — a scenario timeline item takes effect, or stops taking effect.
    Control {
        /// Index into [`crate::scenario::Scenario::events`], in the order the loader
        /// sorted them (by `t`, then by the order written).
        item: u32,
        /// `false` at the item's `t`, `true` at its `until` — the edge that restores a
        /// `demand.multiplier` or ends an `outage`.
        end: bool,
    },
    /// Priority 1 — the periodic mobility step (ADR 0004 decision 2).
    MobilityStep,
    /// Priority 2 — a traffic-signal controller advances.
    SignalPhase {
        /// Which controller.
        signal: SignalId,
    },
    /// Priority 3 — a frame's arrival ends, so its reception outcome is decided.
    ///
    /// **One event per frame, not one per (frame, receiver).** The receiver set is
    /// resolved inside the handler, which then maps over it in parallel and merges in
    /// `NodeId` order — outcomes for all receivers of one frame are independent given the
    /// arrival set (invariant I-R2), so the map is pure. Keeping the fan-out inside the
    /// phase is ADR 0004 decision 5: the global heap holds cross-node events, and one
    /// entry per frame instead of one per neighbour is the difference Appendix A's spike
    /// measured between a heap that scales and one that does not.
    PhyEnd {
        /// The frame on the air.
        frame: FrameSeq,
    },
    /// Priority 4 — a MAC timer: backoff, AIFS or an SPS reservation.
    MacTimer {
        /// The node whose timer it is.
        node: NodeId,
        /// The 5.9 GHz channel number (03-interfaces.md §4's `ChannelId`), not a
        /// recording channel.
        channel: u16,
    },
    /// Priority 5 — a transmission begins, after the MAC decisions at the same instant.
    PhyStart {
        /// The frame going on the air.
        frame: FrameSeq,
        /// Its transmitter.
        tx: NodeId,
    },
    /// Priority 6 — the batched node phase: every node advances one step.
    ///
    /// The phase-parallel counterpart of [`Event::MobilityStep`]. Each node's own queues —
    /// receive, verify, application, transmit, CRL — are drained *inside* the parallel map
    /// (ADR 0004 decision 5), so they never reach the global heap.
    NodePhase,
    /// Priority 6 — one node's own wakeup, which the rest of the run has to be scheduled
    /// for.
    NodeTask {
        /// Whose.
        node: NodeId,
        /// Which.
        task: NodeTask,
    },
    /// Priority 7 — an SDU arrives over backhaul, cellular or the backend network.
    NetDeliver {
        /// The service data unit being delivered.
        sdu: SduId,
        /// Where it lands.
        to: NodeId,
    },
    /// Priority 8 — a protocol timer or batch window.
    FlowTimer {
        /// The flow this timer belongs to (a credential top-up, a CRL distribution, an
        /// MA batch), by the protocol's own numbering.
        flow: u32,
        /// Which step of that flow.
        step: u16,
    },
    /// Priority 9 — an observation of a fully settled instant.
    Observe {
        /// Which observer wants to look.
        what: Observe,
    },
}

/// The node tasks the heap carries.
///
/// A node's *internal* work — draining its verify queue, ageing its neighbour table — is
/// not here: that happens inside the node phase, against the node's own queues. These are
/// the moments the rest of the run has to be woken up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum NodeTask {
    /// The node runtime's periodic step: clock, inbox, verification, generation.
    Step,
    /// A signature check the node started has finished: the node is woken to hand the
    /// message to its applications at that instant (`ObuRuntime::wake_timed`), and does
    /// nothing else.
    Deliver,
    /// A pseudonym change is due.
    PseudonymChange,
    /// A certificate top-up request is due.
    CredentialTopUp,
    /// A reassembly timeout has run out: the node gives up on the fragments still missing
    /// from the SDUs it was reassembling (`crate::frag`).
    Reassembly,
}

/// What an [`EventClass::Observe`] event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Observe {
    /// Metric providers flush their window.
    MetricFlush,
    /// The UI keyframe (`snapshot.keyframe`) is written.
    Keyframe,
    /// Exporters flush.
    ExportFlush,
    /// The run's horizon: the last event, which stops the loop.
    EndOfRun,
}

impl Event {
    /// The class, and therefore the priority, of this payload.
    ///
    /// Total and `const`: the mapping is a compiler-checked match, not a field.
    pub const fn class(&self) -> EventClass {
        match self {
            Event::Control { .. } => EventClass::Control,
            Event::MobilityStep => EventClass::MobilityStep,
            Event::SignalPhase { .. } => EventClass::SignalPhase,
            Event::PhyEnd { .. } => EventClass::PhyEnd,
            Event::MacTimer { .. } => EventClass::MacTimer,
            Event::PhyStart { .. } => EventClass::PhyStart,
            Event::NodePhase | Event::NodeTask { .. } => EventClass::NodeTask,
            Event::NetDeliver { .. } => EventClass::NetDeliver,
            Event::FlowTimer { .. } => EventClass::FlowTimer,
            Event::Observe { .. } => EventClass::Observe,
        }
    }

    /// This payload's priority, `0` (earliest) to `9` (latest).
    pub const fn priority(&self) -> u8 {
        self.class().priority()
    }

    /// The node this event concerns, when it concerns one.
    ///
    /// Used to route an event into the right node-local queue inside the node phase, and
    /// by the run report.
    pub const fn node(&self) -> Option<NodeId> {
        match self {
            Event::MacTimer { node: n, .. }
            | Event::PhyStart { tx: n, .. }
            | Event::NodeTask { node: n, .. }
            | Event::NetDeliver { to: n, .. } => Some(*n),
            Event::Control { .. }
            | Event::MobilityStep
            | Event::SignalPhase { .. }
            | Event::PhyEnd { .. }
            | Event::NodePhase
            | Event::FlowTimer { .. }
            | Event::Observe { .. } => None,
        }
    }

    /// One representative payload of every class, in priority order.
    ///
    /// The fixture the ordering tests and the table test are written against, and the
    /// reason a new class cannot be added without this module noticing.
    pub fn one_of_each() -> [Event; EventClass::ALL.len()] {
        [
            Event::Control {
                item: 0,
                end: false,
            },
            Event::MobilityStep,
            Event::SignalPhase {
                signal: SignalId::new(0),
            },
            Event::PhyEnd {
                frame: FrameSeq::new(0),
            },
            Event::MacTimer {
                node: NodeId::new(0),
                channel: 172,
            },
            Event::PhyStart {
                frame: FrameSeq::new(0),
                tx: NodeId::new(0),
            },
            Event::NodeTask {
                node: NodeId::new(0),
                task: NodeTask::Step,
            },
            Event::NetDeliver {
                sdu: SduId::new(0),
                to: NodeId::new(0),
            },
            Event::FlowTimer { flow: 0, step: 0 },
            Event::Observe {
                what: Observe::MetricFlush,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module's table has a row per class, and [`Event::one_of_each`] is in the same
    /// order. A new `EventClass` variant fails this until it has a payload variant here.
    #[test]
    fn every_class_has_a_payload_variant_in_priority_order() {
        let payloads = Event::one_of_each();
        assert_eq!(payloads.len(), EventClass::ALL.len());
        for (payload, class) in payloads.iter().zip(EventClass::ALL) {
            assert_eq!(payload.class(), class, "payload {payload:?} misclassified");
            assert_eq!(payload.priority(), class.priority());
        }
    }

    /// The documented table in this module's doc comment lists every class by name. A
    /// class added to the enum without a row fails here.
    #[test]
    fn the_documented_table_lists_every_class() {
        let doc = include_str!("event.rs");
        // Only the doc-comment block at the top of the file, so a `match` arm naming the
        // class cannot satisfy this test by accident.
        let table = doc.split("//! # Why the variants").next().expect("header");
        for class in EventClass::ALL {
            let row = format!("| `{class}` |");
            assert!(
                table.contains(&row),
                "no table row for event class {class}; add one to the module documentation"
            );
        }
    }

    /// The batched node phase and a single node's task share the `NodeTask` class, which
    /// is what lets the phase run at priority 6 without inventing a class for it.
    #[test]
    fn the_node_phase_and_a_node_task_share_one_class() {
        assert_eq!(Event::NodePhase.class(), EventClass::NodeTask);
        assert_eq!(
            Event::NodeTask {
                node: NodeId::new(3),
                task: NodeTask::Step,
            }
            .class(),
            EventClass::NodeTask
        );
        // The phase is not addressed to a node; a task is.
        assert_eq!(Event::NodePhase.node(), None);
    }

    /// Payload round-trips through JSON, because the recording's `Control` channel and the
    /// run report both carry it.
    #[test]
    fn payloads_round_trip_through_json() {
        for e in Event::one_of_each() {
            let json = serde_json::to_string(&e).expect("serialise");
            let back: Event = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(e, back);
        }
    }

    /// `node()` answers for exactly the node-addressed classes.
    #[test]
    fn node_addressed_events_name_their_node() {
        for e in Event::one_of_each() {
            let expected = matches!(
                e.class(),
                EventClass::MacTimer
                    | EventClass::PhyStart
                    | EventClass::NodeTask
                    | EventClass::NetDeliver
            );
            assert_eq!(e.node().is_some(), expected, "{e:?}");
        }
    }
}
