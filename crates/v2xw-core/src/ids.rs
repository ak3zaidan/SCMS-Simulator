//! Typed entity ids.
//!
//! All ids are dense `u32` indices assigned at scenario load (and at spawn) in a
//! deterministic order: sorted by scenario declaration, then by spawn time, then by
//! spawn sequence. **Ids are never reused within a run** (03-interfaces.md §1).
//!
//! Every id is `Copy + Ord + Hash + Debug + Display` and serialises transparently as a
//! plain integer. `Ord` matters: parallel phases merge their results in id order and
//! reductions sort contributors by id before summing, which is what makes a run
//! independent of thread count (02-architecture.md §6.4).

use serde::{Deserialize, Serialize};

/// Declares one dense `u32` newtype id with the standard impls.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        #[repr(transparent)]
        pub struct $name(
            /// The dense index. Public so downstream crates can index arrays with it;
            /// prefer [`Self::new`] and [`Self::index`] in new code.
            pub u32,
        );

        impl $name {
            #[doc = concat!("Creates a `", stringify!($name), "` from a dense index.")]
            pub const fn new(index: u32) -> Self {
                Self(index)
            }

            /// The dense index, for use as an array or slice subscript.
            pub const fn index(self) -> u32 {
                self.0
            }

            /// The dense index as `usize`.
            pub const fn as_usize(self) -> usize {
                self.0 as usize
            }
        }

        impl From<u32> for $name {
            fn from(index: u32) -> Self {
                Self(index)
            }
        }

        impl From<$name> for u32 {
            fn from(id: $name) -> u32 {
                id.0
            }
        }

        impl core::fmt::Display for $name {
            #[doc = concat!("Formats as `", $prefix, "<index>`, e.g. `", $prefix, "42`.")]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

define_id!(
    /// A physical thing that moves: vehicle, pedestrian or cyclist.
    ///
    /// An actor may carry zero or more nodes (an OBU, a VRU device); the actor is the
    /// body, the node is the radio/computing device.
    ActorId,
    "a"
);
define_id!(
    /// A communicating/computing entity: OBU, VRU device, RSU, base station, router or
    /// backend entity.
    NodeId,
    "n"
);
define_id!(
    /// A lane of the road network (a centreline polyline with width and attributes).
    LaneId,
    "l"
);
define_id!(
    /// A road edge: the bundle of lanes between two junctions.
    EdgeId,
    "e"
);
define_id!(
    /// A junction of the road network.
    JunctionId,
    "j"
);
define_id!(
    /// A traffic signal (one controller with a phase plan).
    SignalId,
    "sg"
);
define_id!(
    /// A building footprint in the world (an obstacle for propagation and rendering).
    BuildingId,
    "b"
);
define_id!(
    /// A cellular cell.
    ///
    /// Not a spatial-index cell: the uniform grid of [`crate::grid`] addresses its cells
    /// with [`crate::grid::GridCell`], which is a signed coordinate pair rather than a
    /// dense id.
    CellId,
    "c"
);
define_id!(
    /// One service data unit handed down the stack: the application payload that a
    /// generator produced, a codec encoded and a MAC will carry.
    ///
    /// The id is what the radio, network and node crates use to follow one message from
    /// generation to reception without any of them owning the message type. It is
    /// assigned by the node runtime in generation order.
    SduId,
    "sdu"
);
define_id!(
    /// A frame counter on one directed link.
    ///
    /// Keyed together with a [`LinkKey`] it names a single transmission, which is the
    /// scope of a small-scale fading draw ([`crate::rng::EntityRef::LinkFrame`], whose
    /// `frame` field takes `u64::from(seq.index())`). Counting per link rather than
    /// globally is what makes a fading sample independent of how busy the rest of the
    /// world was.
    FrameSeq,
    "f"
);
define_id!(
    /// A node hardware profile: CPU, HSM, memory and storage behaviour
    /// (03-interfaces.md §8, 06-node-models.md §1).
    ///
    /// Profiles are declared once per scenario and referenced by every node that runs on
    /// that hardware, so a cost table is stored once rather than per node.
    HwProfileId,
    "hw"
);
define_id!(
    /// An RSU or cell site: a fixed mast with a position, an antenna height and the nodes
    /// mounted on it (`World::sites`, 03-interfaces.md §2).
    SiteId,
    "site"
);
define_id!(
    /// A pedestrian or cycle crossing of the road network
    /// (`RoadNetwork::crossings`, 03-interfaces.md §2).
    CrossingId,
    "cr"
);
define_id!(
    /// A land-use zone: the polygon that gives a point its propagation environment preset
    /// and its rendering class (`World::landuse`, 03-interfaces.md §2).
    LanduseId,
    "lu"
);
define_id!(
    /// A connection between two lanes through a junction — one movement of the lane graph
    /// (`RoadNetwork::successors`, 03-interfaces.md §2).
    ///
    /// Distinct from the lanes it joins: a connection carries the movement's own
    /// attributes (permitted classes, priority, conflict set, internal geometry).
    ConnectionId,
    "cn"
);

/// A directed radio link, ordered `(tx, rx)`.
///
/// Per-link state — small-scale fading, shadowing correlation, per-link RNG streams — is
/// keyed by this pair. The order matters: `LinkKey(a, b)` and `LinkKey(b, a)` are
/// different keys, because the transmitter's and receiver's antenna heights, patterns
/// and environments differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LinkKey(
    /// Transmitter.
    pub NodeId,
    /// Receiver.
    pub NodeId,
);

impl LinkKey {
    /// Creates a link key from transmitter and receiver.
    pub const fn new(tx: NodeId, rx: NodeId) -> Self {
        Self(tx, rx)
    }

    /// The transmitting node.
    pub const fn tx(self) -> NodeId {
        self.0
    }

    /// The receiving node.
    pub const fn rx(self) -> NodeId {
        self.1
    }

    /// The link in the opposite direction, `(rx, tx)`.
    pub const fn reversed(self) -> Self {
        Self(self.1, self.0)
    }
}

impl core::fmt::Display for LinkKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}->{}", self.0, self.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_and_accessors() {
        let a = ActorId::new(7);
        assert_eq!(a.index(), 7);
        assert_eq!(a.as_usize(), 7);
        assert_eq!(u32::from(a), 7);
        assert_eq!(ActorId::from(7u32), a);
        assert_eq!(a.0, 7);
    }

    #[test]
    fn displays_with_prefix() {
        assert_eq!(ActorId::new(1).to_string(), "a1");
        assert_eq!(NodeId::new(2).to_string(), "n2");
        assert_eq!(LaneId::new(3).to_string(), "l3");
        assert_eq!(EdgeId::new(4).to_string(), "e4");
        assert_eq!(JunctionId::new(5).to_string(), "j5");
        assert_eq!(SignalId::new(6).to_string(), "sg6");
        assert_eq!(BuildingId::new(7).to_string(), "b7");
        assert_eq!(CellId::new(8).to_string(), "c8");
        assert_eq!(
            LinkKey::new(NodeId::new(1), NodeId::new(2)).to_string(),
            "n1->n2"
        );
    }

    /// Every prefix is distinct, so an id in a log or an error message names its own type.
    #[test]
    fn the_later_ids_display_with_their_own_prefixes() {
        assert_eq!(SduId::new(1).to_string(), "sdu1");
        assert_eq!(FrameSeq::new(2).to_string(), "f2");
        assert_eq!(HwProfileId::new(3).to_string(), "hw3");
        assert_eq!(SiteId::new(4).to_string(), "site4");
        assert_eq!(CrossingId::new(5).to_string(), "cr5");
        assert_eq!(LanduseId::new(6).to_string(), "lu6");
        assert_eq!(ConnectionId::new(7).to_string(), "cn7");

        let prefixes = [
            "a", "n", "l", "e", "j", "sg", "b", "c", "sdu", "f", "hw", "site", "cr", "lu", "cn",
        ];
        let mut sorted = prefixes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), prefixes.len(), "two ids share a prefix");
    }

    /// The new ids carry the same helpers and the same transparent encoding as the old
    /// ones, because a downstream crate indexes arrays with them and serialises them into
    /// records.
    #[test]
    fn the_later_ids_have_the_standard_helpers() {
        let s = SduId::new(9);
        assert_eq!(s.index(), 9);
        assert_eq!(s.as_usize(), 9);
        assert_eq!(u32::from(s), 9);
        assert_eq!(SduId::from(9u32), s);
        assert_eq!(serde_json::to_string(&s).unwrap(), "9");
        assert_eq!(serde_json::from_str::<SduId>("9").unwrap(), s);

        let mut v = [
            ConnectionId::new(2),
            ConnectionId::new(0),
            ConnectionId::new(1),
        ];
        v.sort();
        assert_eq!(v[0], ConnectionId::new(0));
        assert_eq!(v[2], ConnectionId::new(2));

        // The per-link frame counter feeds the RNG's single-use scope as a u64.
        let frame = FrameSeq::new(4_000_000_000);
        assert_eq!(u64::from(frame.index()), 4_000_000_000u64);
    }

    #[test]
    fn ordering_is_by_index() {
        let mut v = vec![NodeId::new(3), NodeId::new(1), NodeId::new(2)];
        v.sort();
        assert_eq!(v, vec![NodeId::new(1), NodeId::new(2), NodeId::new(3)]);
    }

    #[test]
    fn link_key_is_directed() {
        let fwd = LinkKey::new(NodeId::new(1), NodeId::new(2));
        let rev = fwd.reversed();
        assert_ne!(fwd, rev);
        assert_eq!(rev, LinkKey::new(NodeId::new(2), NodeId::new(1)));
        assert_eq!(fwd.tx(), NodeId::new(1));
        assert_eq!(fwd.rx(), NodeId::new(2));
        assert!(fwd < rev);
    }

    #[test]
    fn serde_is_transparent() {
        assert_eq!(serde_json::to_string(&ActorId::new(9)).unwrap(), "9");
        assert_eq!(
            serde_json::from_str::<ActorId>("9").unwrap(),
            ActorId::new(9)
        );
        assert_eq!(
            serde_json::to_string(&LinkKey::new(NodeId::new(1), NodeId::new(2))).unwrap(),
            "[1,2]"
        );
    }
}
