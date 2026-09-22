//! Backend service models: the reason provisioning costs something.
//!
//! 06-node-models §4 says it plainly — "nothing in a backend entity is instantaneous". An
//! entity here is a `c`-server queue: a request occupies one server for a service time
//! composed from the cryptographic operations it performs plus a fixed per-request
//! overhead, and a request that arrives while every server is busy waits. That is the
//! `medium` tier's structure with a deterministic service time; the `high` tier's
//! log-normal per-request distribution and its availability model are a documented hook
//! (`ServiceModelSpec::availability`), not built, because their parameters are
//! `todo-calibrate` in the design and inventing them would be worse than leaving the seam.
//!
//! Determinism: no RNG, no float. Service times are sums of the integer nanosecond
//! durations of `v2xw_sec::primitive`'s cost tables, and the server a request lands on is
//! the lowest-indexed one that is free earliest, chosen by an explicit total order.

use v2xw_core::time::{Duration, SimTime};

/// The batching windows of 06-node-models §4: release when either bound is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchPolicy {
    /// Release when this many items are held.
    pub max_items: u32,
    /// Release when the oldest held item is this old.
    pub max_delay: Duration,
}

impl BatchPolicy {
    /// The CAMP PoC shuffle: 10,000 requests or one day, whichever comes first.
    ///
    /// [CAMP-EE §2.2.7 for the certificate-request shuffle; SCMS-765 for the report
    /// shuffle; restated in 05-protocols §3.2 and 06-node-models §4.]
    pub const CAMP_SHUFFLE: BatchPolicy = BatchPolicy {
        max_items: 10_000,
        max_delay: Duration::from_secs(86_400),
    };
}

/// How an entity serves requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceModelSpec {
    /// Servers, the `c` of the M/M/c of 06-node-models §4.
    pub servers: u32,
    /// Per-request overhead on top of the cryptographic cost.
    pub fixed_overhead: Duration,
    /// The batching window, where the entity has one.
    pub batching: Option<BatchPolicy>,
    /// Availability: a documented hook. `None` everywhere in this crate, because
    /// 06-node-models §4 leaves the two-state Markov parameters `todo-calibrate` and this
    /// crate does not invent them.
    pub availability: Option<()>,
}

impl ServiceModelSpec {
    /// A `c`-server entity with the given per-request overhead and no batching.
    pub const fn new(servers: u32, fixed_overhead: Duration) -> ServiceModelSpec {
        ServiceModelSpec {
            servers,
            fixed_overhead,
            batching: None,
            availability: None,
        }
    }

    /// The same, with a batching window.
    #[must_use]
    pub const fn batched(self, policy: BatchPolicy) -> ServiceModelSpec {
        ServiceModelSpec {
            batching: Some(policy),
            ..self
        }
    }
}

/// A live `c`-server queue.
///
/// Holds one "free at" instant per server. A request arriving at `t` takes the server
/// that becomes free earliest; ties go to the lowest index, which is an explicit total
/// order rather than whatever order a map happened to iterate in.
#[derive(Debug, Clone)]
pub struct ServiceQueue {
    free_at: Vec<SimTime>,
    fixed_overhead: Duration,
    busy_ns: u64,
    served: u64,
    waited_ns: u64,
}

impl ServiceQueue {
    /// A queue for `spec`.
    pub fn new(spec: &ServiceModelSpec) -> ServiceQueue {
        ServiceQueue {
            free_at: vec![0; spec.servers.max(1) as usize],
            fixed_overhead: spec.fixed_overhead,
            busy_ns: 0,
            served: 0,
            waited_ns: 0,
        }
    }

    /// Admits a request that arrived at `arrival` and needs `work` of cryptographic time.
    ///
    /// Returns the instant the entity finishes it — which is when its outgoing messages
    /// leave and when its stages are stamped.
    pub fn admit(&mut self, arrival: SimTime, work: Duration) -> SimTime {
        let (index, free) = self
            .free_at
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|&(i, t)| (t, i))
            .unwrap_or((0, arrival));
        let start = free.max(arrival);
        let service = Duration::from_nanos(
            work.as_nanos()
                .saturating_add(self.fixed_overhead.as_nanos()),
        );
        let done = service.after(start);
        self.free_at[index] = done;
        self.busy_ns = self.busy_ns.saturating_add(service.as_nanos());
        self.waited_ns = self.waited_ns.saturating_add(start - arrival);
        self.served += 1;
        done
    }

    /// How many requests this entity has served.
    pub const fn served(&self) -> u64 {
        self.served
    }

    /// Total time its servers were busy.
    pub const fn busy(&self) -> Duration {
        Duration::from_nanos(self.busy_ns)
    }

    /// Total time requests spent waiting for a free server.
    pub const fn waiting(&self) -> Duration {
        Duration::from_nanos(self.waited_ns)
    }
}
