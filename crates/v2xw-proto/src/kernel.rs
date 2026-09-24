//! The deterministic event kernel the flows run on.
//!
//! A protocol plug-in in the finished engine returns `Action`s and the engine routes them
//! (03-interfaces §7). The engine's loop is being built in `v2xw-engine` and its event
//! payload type does not exist yet, so this crate carries the smallest kernel that makes
//! the same guarantees — every message crosses a link with bytes (I-P1) and every compute
//! step is charged to a queue (I-P2) — and the engine adopts the entities unchanged by
//! implementing the same dispatch over its own scheduler. [`Outbox`] is the accumulator
//! form of §7's `Action` list: [`Outbox::send`] is `Send`, [`Outbox::compute`] is
//! `Compute`, [`Outbox::start_timer`] is `StartTimer`, [`Outbox::stage`] is `Emit`.
//!
//! **Determinism.** Events are ordered by `(time, sequence number)`, the sequence number
//! being the order in which events were created. That is a total order, so no two runs of
//! the same scenario can dispatch in different orders. No wall clock is read anywhere; no
//! `HashMap` is iterated anywhere; every service time is an integer number of nanoseconds.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};
use v2xw_sec::primitive::{PrimitiveId, PrimitiveOpKind};

use crate::error::{ProtoError, Result};
use crate::net::{BackendNet, Transport};
use crate::service::{ServiceModelSpec, ServiceQueue};
use crate::sizes::WireSize;
use crate::spec::OpDescriptor;
use crate::stage::{FlowId, FlowRun, StageId, StageLog, StageStamp, WireStep};

/// A message on its way to an entity.
#[derive(Debug, Clone)]
pub struct Delivery<M> {
    /// When it arrives.
    pub at: SimTime,
    /// Who sent it.
    pub from: NodeId,
    /// Who receives it.
    pub to: NodeId,
    /// The message.
    pub msg: M,
    /// Which flow it belongs to.
    pub flow: FlowId,
    /// Which run of that flow.
    pub run: FlowRun,
}

#[derive(Debug)]
struct Queued<M> {
    at: SimTime,
    seq: u64,
    delivery: Delivery<M>,
}

impl<M> PartialEq for Queued<M> {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}
impl<M> Eq for Queued<M> {}
impl<M> PartialOrd for Queued<M> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<M> Ord for Queued<M> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.at, self.seq).cmp(&(other.at, other.seq))
    }
}

struct PendingSend<M> {
    to: NodeId,
    msg: M,
    step: &'static str,
    size: WireSize,
    transport: Transport,
    flow: FlowId,
    run: FlowRun,
}

struct PendingTimer<M> {
    delay: Duration,
    msg: M,
    flow: FlowId,
    run: FlowRun,
}

struct PendingStage {
    stage: StageId,
    node: Option<NodeId>,
    size: Option<u32>,
    flow: FlowId,
    run: FlowRun,
}

/// What an entity produced while handling one message.
///
/// Built empty, filled by the entity, consumed by [`Kernel::dispatch`]. The entity never
/// sees a clock: it says what it did and what it wants sent, and the kernel decides when
/// those things happen, which is what keeps I-P2 true — an entity cannot give itself a
/// free computation by forgetting to look at the time.
pub struct Outbox<M> {
    profile: &'static str,
    work: Duration,
    ops: BTreeMap<(&'static str, &'static str), u64>,
    sends: Vec<PendingSend<M>>,
    timers: Vec<PendingTimer<M>>,
    stages: Vec<PendingStage>,
}

impl<M> Outbox<M> {
    /// An empty outbox charging against `profile`'s cost anchors.
    pub fn new(profile: &'static str) -> Outbox<M> {
        Outbox {
            profile,
            work: Duration::ZERO,
            ops: BTreeMap::new(),
            sends: Vec::new(),
            timers: Vec::new(),
            stages: Vec::new(),
        }
    }

    /// Charges a costed operation (`Action::Compute`).
    pub fn compute(&mut self, op: OpDescriptor) {
        self.work = Duration::from_nanos(
            self.work
                .as_nanos()
                .saturating_add(op.charge(self.profile).as_nanos()),
        );
        *self
            .ops
            .entry((op.primitive.as_str(), op_kind_str(op.kind)))
            .or_insert(0) += u64::from(op.count);
    }

    /// Charges `count` operations of `kind` on `primitive`.
    pub fn charge(&mut self, primitive: PrimitiveId, kind: PrimitiveOpKind, count: u32) {
        self.compute(OpDescriptor::new(primitive, kind, count));
    }

    /// Queues a message (`Action::Send`).
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        to: NodeId,
        msg: M,
        step: &'static str,
        size: WireSize,
        transport: Transport,
        flow: FlowId,
        run: FlowRun,
    ) {
        self.sends.push(PendingSend {
            to,
            msg,
            step,
            size,
            transport,
            flow,
            run,
        });
    }

    /// Queues a message to this entity itself, `delay` after it finishes this step
    /// (`Action::StartTimer`). It crosses no link and costs no bytes.
    pub fn start_timer(&mut self, delay: Duration, msg: M, flow: FlowId, run: FlowRun) {
        self.timers.push(PendingTimer {
            delay,
            msg,
            flow,
            run,
        });
    }

    /// Stamps a stage (`Action::Emit`) at this step's completion.
    pub fn stage(&mut self, stage: StageId, flow: FlowId, run: FlowRun) {
        self.stages.push(PendingStage {
            stage,
            node: None,
            size: None,
            flow,
            run,
        });
    }

    /// Stamps a per-node stage, optionally with the artefact's size.
    pub fn stage_at(
        &mut self,
        stage: StageId,
        node: NodeId,
        size: Option<u32>,
        flow: FlowId,
        run: FlowRun,
    ) {
        self.stages.push(PendingStage {
            stage,
            node: Some(node),
            size,
            flow,
            run,
        });
    }

    /// The cryptographic time charged so far.
    pub const fn work(&self) -> Duration {
        self.work
    }
}

const fn op_kind_str(kind: PrimitiveOpKind) -> &'static str {
    match kind {
        PrimitiveOpKind::KeyGen => "keygen",
        PrimitiveOpKind::Sign => "sign",
        PrimitiveOpKind::Verify => "verify",
    }
}

/// The event kernel: a schedule, one service queue per entity, and the two logs.
pub struct Kernel<M> {
    now: SimTime,
    seq: u64,
    heap: BinaryHeap<Reverse<Queued<M>>>,
    queues: BTreeMap<NodeId, ServiceQueue>,
    profiles: BTreeMap<NodeId, &'static str>,
    net: BackendNet,
    /// Every stage every flow stamped.
    pub stages: StageLog,
    /// Every hop, as `proto.msg` records it.
    pub steps: Vec<WireStep>,
    /// Operation counts per node, for the protocol metrics.
    pub ops: BTreeMap<(NodeId, &'static str, &'static str), u64>,
}

impl<M> Kernel<M> {
    /// A kernel over `net` with no entities yet, with its clock at zero.
    pub fn new(net: BackendNet) -> Kernel<M> {
        Kernel::new_at(net, 0)
    }

    /// A kernel whose clock starts at `t0`.
    ///
    /// The engine's clock does not start at zero — a scenario's `time.t0` puts the run on
    /// a calendar, and a vehicle that spawns 1.5 s into the run must ask for credentials
    /// at 1.5 s and not at 0. `t0` is a [`SimTime`] the caller supplies; nothing here
    /// reads a clock to obtain it.
    pub fn new_at(net: BackendNet, t0: SimTime) -> Kernel<M> {
        Kernel {
            now: t0,
            seq: 0,
            heap: BinaryHeap::new(),
            queues: BTreeMap::new(),
            profiles: BTreeMap::new(),
            net,
            stages: StageLog::new(),
            steps: Vec::new(),
            ops: BTreeMap::new(),
        }
    }

    /// Gives `node` a service model and a hardware profile.
    pub fn host(&mut self, node: NodeId, spec: &ServiceModelSpec, profile: &'static str) {
        self.queues.insert(node, ServiceQueue::new(spec));
        self.profiles.insert(node, profile);
    }

    /// The hardware profile `node`'s costs are charged against.
    pub fn profile_of(&self, node: NodeId) -> &'static str {
        self.profiles.get(&node).copied().unwrap_or("")
    }

    /// The current instant.
    pub const fn now(&self) -> SimTime {
        self.now
    }

    /// The network.
    pub const fn net(&self) -> &BackendNet {
        &self.net
    }

    /// The network, mutably: how a device joining mid-run gets its links.
    pub const fn net_mut(&mut self) -> &mut BackendNet {
        &mut self.net
    }

    /// The instant `node` will have finished everything already given to it.
    ///
    /// The clock alone is not that instant. [`Kernel::now`] is the *arrival* time of the
    /// delivery being dispatched, and an entity's stages are stamped at the completion of
    /// its service time, so an externally triggered action injected at `now` can be
    /// stamped before work the entity was already doing. That is not an early message, it
    /// is a stage inversion: a Misbehaviour Authority's `decision` appearing before the
    /// `report_received` that caused it. [`Kernel::inject_at`] clamps to this.
    pub fn busy_until(&self, node: NodeId) -> SimTime {
        let free = self
            .queues
            .get(&node)
            .map_or(0, crate::service::ServiceQueue::busy_until);
        free.max(self.now)
    }

    /// The service queue of `node`, for the telemetry a run reports.
    pub fn queue(&self, node: NodeId) -> Option<&ServiceQueue> {
        self.queues.get(&node)
    }

    /// Injects a message with no link delay — how a flow starts.
    ///
    /// `at` is taken as given: the caller is the kernel's own dispatch or a test placing an
    /// event on the schedule deliberately. A driver that is triggering an entity from
    /// outside wants [`Kernel::inject_at`], which clamps.
    pub fn inject(&mut self, at: SimTime, delivery: Delivery<M>) {
        self.push(at, delivery);
    }

    /// Injects at the earliest instant that cannot invert a stage at the receiver.
    ///
    /// `max(at, busy_until(to))`. It only ever moves an injection later, and only past
    /// work the entity had already been given, so it cannot hide a flow from a horizon —
    /// it can only stop a flow from starting inside another one.
    ///
    /// Returns the instant the delivery was scheduled for.
    pub fn inject_at(&mut self, at: SimTime, delivery: Delivery<M>) -> SimTime {
        let at = at.max(self.busy_until(delivery.to));
        let mut delivery = delivery;
        delivery.at = at;
        self.push(at, delivery);
        at
    }

    fn push(&mut self, at: SimTime, delivery: Delivery<M>) {
        self.seq += 1;
        self.heap.push(Reverse(Queued {
            at,
            seq: self.seq,
            delivery,
        }));
    }

    /// The next delivery, advancing the clock to it.
    ///
    /// Not an [`Iterator`]: advancing the kernel needs `&mut self` *and* the caller must be
    /// able to call [`Kernel::dispatch`] between two pops, which an iterator's borrow would
    /// forbid.
    pub fn next_delivery(&mut self) -> Option<Delivery<M>> {
        let Reverse(q) = self.heap.pop()?;
        self.now = q.at;
        Some(q.delivery)
    }

    /// The next delivery, but only if it is due at or before `horizon`.
    ///
    /// This is what makes the deployment drivable from a foreign event loop: the engine
    /// advances its own clock to `t` and hands it here, and the backend does exactly the
    /// work that was due by then and no more. Without it the only way to advance a flow
    /// is to run it to quiescence, which would let a provisioning round trip complete
    /// inside one engine step and cost nothing.
    ///
    /// The clock is advanced only when something is popped, so a horizon in the middle of
    /// a quiet interval does not move the backend's `now` past the engine's.
    pub fn next_delivery_before(&mut self, horizon: SimTime) -> Option<Delivery<M>> {
        let due = self.heap.peek().map(|Reverse(q)| q.at)?;
        if due > horizon {
            return None;
        }
        self.next_delivery()
    }

    /// When the next scheduled delivery is due, if anything is scheduled.
    pub fn next_due(&self) -> Option<SimTime> {
        self.heap.peek().map(|Reverse(q)| q.at)
    }

    /// How many deliveries are still scheduled.
    pub fn pending(&self) -> usize {
        self.heap.len()
    }

    /// The stage stamps made since `cursor`, and the new cursor.
    ///
    /// The log keeps everything — a decomposition is a query over the whole run — so a
    /// recorder that must emit each stamp exactly once carries a cursor rather than the
    /// log being drained under it.
    pub fn stages_since(&self, cursor: usize) -> (&[StageStamp], usize) {
        let all = self.stages.stamps();
        let from = cursor.min(all.len());
        (&all[from..], all.len())
    }

    /// The wire steps made since `cursor`, and the new cursor.
    pub fn steps_since(&self, cursor: usize) -> (&[WireStep], usize) {
        let from = cursor.min(self.steps.len());
        (&self.steps[from..], self.steps.len())
    }

    /// Charges an entity's work, stamps its stages and schedules what it sent.
    ///
    /// Returns the instant the entity finished, which is when its stages were stamped and
    /// when its messages went on the wire.
    ///
    /// # Errors
    /// [`ProtoError::NoEntity`] if the node was never given a service model, and
    /// [`ProtoError::NoLink`] if it sent to a node it has no link to. Both are modelling
    /// defects: refusing is the point, since the alternative is a message that arrives for
    /// free and a measurement that means nothing.
    pub fn dispatch(&mut self, at: SimTime, node: NodeId, out: Outbox<M>) -> Result<SimTime> {
        let queue = self
            .queues
            .get_mut(&node)
            .ok_or(ProtoError::NoEntity { node })?;
        let done = queue.admit(at, out.work);

        for ((primitive, kind), count) in out.ops {
            *self.ops.entry((node, primitive, kind)).or_insert(0) += count;
        }

        for s in out.stages {
            self.stages.push(StageStamp {
                t: done,
                run: s.run,
                flow: s.flow,
                stage: s.stage,
                node: s.node,
                size: s.size,
            });
        }

        for t in out.timers {
            let delivery = Delivery {
                at: t.delay.after(done),
                from: node,
                to: node,
                msg: t.msg,
                flow: t.flow,
                run: t.run,
            };
            self.push(t.delay.after(done), delivery);
        }

        for s in out.sends {
            let link = self.net.link(node, s.to).ok_or(ProtoError::NoLink {
                from: node,
                to: s.to,
            })?;
            let bytes = s.size.bytes();
            // A handler names the device's access leg as the cellular uplink because that
            // is the deployment's default; the link a driver actually gave the device
            // (`ScmsRun::set_access`) says which access carried it — a roadside relay's
            // backhaul, say — and the bytes belong in that bucket.
            let transport = if s.transport == Transport::CellularUu {
                link.transport
            } else {
                s.transport
            };
            self.steps.push(WireStep {
                t: done,
                from: node,
                to: s.to,
                flow: s.flow,
                run: s.run,
                step: s.step,
                bytes,
                transport,
            });
            let arrival = link.delay(bytes).after(done);
            let delivery = Delivery {
                at: arrival,
                from: node,
                to: s.to,
                msg: s.msg,
                flow: s.flow,
                run: s.run,
            };
            self.push(arrival, delivery);
        }

        Ok(done)
    }

    /// Total bytes carried, by transport — the `topup.bytes` and `crl.bytes` metrics.
    pub fn bytes_by_transport(&self) -> BTreeMap<Transport, u64> {
        let mut out: BTreeMap<Transport, u64> = BTreeMap::new();
        for s in &self.steps {
            *out.entry(s.transport).or_insert(0) += u64::from(s.bytes);
        }
        out
    }

    /// Total bytes carried by one flow.
    pub fn bytes_of_flow(&self, flow: FlowId) -> u64 {
        self.steps
            .iter()
            .filter(|s| s.flow == flow)
            .map(|s| u64::from(s.bytes))
            .sum()
    }
}
