//! The links protocol messages actually cross.
//!
//! Invariant I-P1 (05-protocols §7) is "no direct entity-to-entity calls": every message
//! crosses a modelled link with bytes. This module is the smallest thing that can be true
//! of: a per-link one-way latency and a bandwidth, so a 400 kB CRL and a 40 B query do not
//! take the same time to arrive.
//!
//! Neither number is published anywhere in the design set for the SCMS backend, so both
//! are `todo-calibrate` card parameters with plans, exactly as 06-node-models §4 says the
//! M/M/c parameters are. What is *not* a parameter is the shape: `latency + bytes·8 /
//! bandwidth`, store-and-forward, one direction at a time.

use std::collections::BTreeMap;

use v2xw_core::ids::NodeId;
use v2xw_core::time::Duration;

/// Which transport carried a message.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Transport {
    /// The wired network between backend entities.
    BackendNet,
    /// The cellular uplink/downlink between a device and the backend.
    CellularUu,
    /// A roadside unit's backhaul, used when a device is provisioned through an RSU.
    RsuBackhaul,
    /// The 5.9 GHz air interface (CRL broadcast, epidemic exchange).
    V2xAir,
}

impl Transport {
    /// The transport's stable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::BackendNet => "backend-net",
            Transport::CellularUu => "cellular-uu",
            Transport::RsuBackhaul => "rsu-backhaul",
            Transport::V2xAir => "v2x-air",
        }
    }
}

/// One directed link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// One-way latency.
    pub latency: Duration,
    /// Bandwidth in bits per second. Zero means "unmodelled", and only the latency counts.
    pub bandwidth_bps: u64,
    /// Which transport this link is.
    pub transport: Transport,
}

impl Link {
    /// The time to move `bytes` across this link: latency plus serialisation.
    ///
    /// Integer arithmetic throughout — nanoseconds and bytes — so there is no float to
    /// quantise and no rounding that depends on the platform's libm.
    pub const fn delay(&self, bytes: u32) -> Duration {
        if self.bandwidth_bps == 0 {
            return self.latency;
        }
        let bits = (bytes as u64).saturating_mul(8);
        let ns = bits
            .saturating_mul(1_000_000_000)
            .wrapping_div(self.bandwidth_bps);
        Duration::from_nanos(self.latency.as_nanos().saturating_add(ns))
    }
}

/// The links between the entities of a deployment.
///
/// A [`BTreeMap`] keyed by the ordered pair, so the iteration order of the topology can
/// never reach an output. An absent link is a modelling error, not a silent zero-latency
/// hop: [`BackendNet::link`] returns `None` and the kernel refuses to deliver.
#[derive(Debug, Clone, Default)]
pub struct BackendNet {
    links: BTreeMap<(NodeId, NodeId), Link>,
}

impl BackendNet {
    /// An empty topology.
    pub fn new() -> BackendNet {
        BackendNet {
            links: BTreeMap::new(),
        }
    }

    /// Adds a link in both directions.
    pub fn connect(&mut self, a: NodeId, b: NodeId, link: Link) {
        self.links.insert((a, b), link);
        self.links.insert((b, a), link);
    }

    /// The link from `from` to `to`, if there is one.
    pub fn link(&self, from: NodeId, to: NodeId) -> Option<Link> {
        self.links.get(&(from, to)).copied()
    }

    /// How many directed links the topology holds.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// True if nothing is connected.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}
