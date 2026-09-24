//! One frame on the air, layer by layer: what every octet of a PSDU is for.
//!
//! A signed safety message is not what goes on the air. Below the facilities-layer payload
//! and its IEEE 1609.2 / TS 103 097 envelope sit a network and transport header (WSMP, or
//! GeoNetworking plus BTP), an LLC/SNAP header, the 802.11 MAC header and the frame check
//! sequence. The PHY's air time is a function of the whole PSDU, and a measurement of
//! "security overhead" or "header overhead" is a ratio between these layers, so the split
//! has to be computed once, in one place, and carried with the frame.
//!
//! [`FrameLayers`] is that split. Its fields are disjoint and their sum is the PSDU length
//! ([`FrameLayers::psdu_bytes`]), which is invariant I-N1 applied *inside* the air bucket:
//! every octet on the air belongs to exactly one layer. `frame_layers_partition_the_psdu`
//! is the test that holds it.
//!
//! # The sizes, and how sure each one is
//!
//! | Layer | Octets | Source |
//! |---|---|---|
//! | Payload | the encoder's own output | `v2xw-msg` (UPER, VERIFIED against pycrate) |
//! | Security envelope | the envelope's own output | `v2xw-sec` (COER, byte-exact, Phase 1 criterion 2) |
//! | WSMP (BSM) | 5 | IEEE 1609.3, 04-models.md §7.1, VERIFIED |
//! | GN SHB + BTP-B (CAM) | 44 | EN 302 636-4-1 §9.8.4, EN 302 636-5-1 §7.2, 04-models.md §7.2, VERIFIED |
//! | LLC/SNAP | 8 | IEEE 802.2 + SNAP, 04-models.md §4.6, DERIVED |
//! | 802.11 MAC header, QoS Data | 26 | IEEE 802.11-2020 §9.3.2.1, 04-models.md §4.6: clause UNVERIFIED |
//! | FCS | 4 | IEEE 802.11-2020 §9.2.4.8, 04-models.md §4.6: standard constant, not independently verified |
//!
//! Two uncertainties are stated rather than hidden, and both are on the model card:
//!
//! * **LLC framing under WSMP.** 04-models.md §4.6 gives LLC/SNAP (8 octets, EtherType
//!   `0x88DC`) for both stacks. IEEE 1609.3-2016 also admits EtherType Protocol
//!   Discrimination, which is two octets. The NDSS 2024 accounting (§V-D) puts 40 octets
//!   around an SPDU, which sits between the two readings (26 + 4 + 5 + 2 = 37 against
//!   26 + 4 + 5 + 8 = 43). This module uses the design's 8.
//! * **QoS Data rather than Data.** EDCA access categories are carried in the QoS Control
//!   field, so a frame queued at AC_VO is a QoS Data frame (26 octets, not 24). The clause
//!   number is UNVERIFIED in 04-models.md §4.6.
//!
//! The physical layer's own overhead — preamble, SIGNAL field, SERVICE and tail bits, pad —
//! is air *time*, not octets, and is `v2xw_radio::air_time`'s to add. Nothing here counts
//! it, so the byte totals and the airtime totals are two different measures of one frame.

use serde::{Deserialize, Serialize};

use crate::gn::{GN_BTP_NET_LAYER_ID, GnBtpNetLayer};
use crate::netlayer::{BtpKind, BtpPort, GnTransport, LLC_SNAP_BYTES, NetMeta, Psid};
use crate::wsmp::{WSMP_NET_LAYER_ID, WsmpNetLayer};

/// The 802.11 MAC header of a QoS Data frame, octets: frame control 2, duration 2, three
/// addresses 18, sequence control 2, QoS control 2.
///
/// [IEEE 802.11-2020 §9.3.2.1; 04-models.md §4.6, clause UNVERIFIED]. QoS Data rather than
/// Data because the EDCA access category rides in the QoS Control field.
pub const MAC_HEADER_QOS_DATA_BYTES: u32 = 26;

/// The 802.11 frame check sequence, octets: a CRC-32.
///
/// [IEEE 802.11-2020 §9.2.4.8; 04-models.md §4.6, "standard constant, not independently
/// verified"].
pub const FCS_BYTES: u32 = 4;

/// The largest MSDU 802.11 carries, octets [04-models.md §4.6: "well-established constant;
/// clause UNVERIFIED"]. An SDU above it has to be fragmented above the network layer,
/// because 802.11 never fragments a group-addressed frame.
pub const MAX_MSDU_BYTES: u32 = 2_304;

/// Which network and transport stack a node runs (the scenario's `net.layer`).
#[derive(Debug, Clone)]
pub enum NetStack {
    /// IEEE 1609.3 WSMP, the US stack (`wsmp`).
    Wsmp(WsmpNetLayer),
    /// ETSI GeoNetworking with BTP, the European stack (`gn-btp`).
    GnBtp(GnBtpNetLayer),
}

impl NetStack {
    /// The stack a scenario's `net.layer` names, with its standard defaults, or `None`
    /// for a name that is neither `wsmp` nor `gn-btp` — which the scenario loader has
    /// already refused, so a caller that gets `None` is holding an unvalidated scenario.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "wsmp" => Some(NetStack::Wsmp(WsmpNetLayer::default())),
            "gn-btp" => Some(NetStack::GnBtp(GnBtpNetLayer::default())),
            _ => None,
        }
    }

    /// The model id of the layer, for a manifest and a record.
    pub const fn model_id(&self) -> &'static str {
        match self {
            NetStack::Wsmp(_) => WSMP_NET_LAYER_ID,
            NetStack::GnBtp(_) => GN_BTP_NET_LAYER_ID,
        }
    }

    /// The scenario spelling of the layer.
    pub const fn name(&self) -> &'static str {
        match self {
            NetStack::Wsmp(_) => "wsmp",
            NetStack::GnBtp(_) => "gn-btp",
        }
    }

    /// The largest SDU the network layer accepts, octets — the MTU a fragmenter splits to.
    pub const fn sdu_mtu(&self) -> u32 {
        match self {
            NetStack::Wsmp(w) => w.payload_mtu(),
            NetStack::GnBtp(g) => g.sdu_mtu(),
        }
    }

    /// The meta a message of `msg` is sent with on this stack, for an SDU of `sdu_bytes`.
    ///
    /// On WSMP every safety message is addressed by PSID `0x20` with no N-header
    /// extensions (04-models.md §7.1). On GeoNetworking a DENM is a GeoBroadcast to port
    /// 2002 and everything else a single-hop broadcast to port 2001 (§7.2). The LLC/SNAP
    /// octets are **excluded** here in both cases and counted once, by
    /// [`FrameLayers::llc_snap`], so that the two stacks' framing is compared like for
    /// like.
    pub const fn meta(&self, msg: FrameMsg, sdu_bytes: u32) -> NetMeta {
        match self {
            NetStack::Wsmp(_) => match msg {
                FrameMsg::Safety | FrameMsg::Denm => {
                    NetMeta::for_bsm(sdu_bytes).with_llc_snap(false)
                }
                FrameMsg::Spat | FrameMsg::Map | FrameMsg::Srm | FrameMsg::Ssm => {
                    NetMeta::wsmp(sdu_bytes, Psid::INTERSECTION).with_llc_snap(false)
                }
            },
            NetStack::GnBtp(_) => match msg {
                FrameMsg::Denm => NetMeta::for_denm(sdu_bytes).with_llc_snap(false),
                FrameMsg::Safety => NetMeta::for_cam(sdu_bytes).with_llc_snap(false),
                FrameMsg::Spat => {
                    NetMeta::gn(sdu_bytes, GnTransport::Shb, BtpKind::B, BtpPort::SPATEM)
                        .with_llc_snap(false)
                }
                FrameMsg::Map => {
                    NetMeta::gn(sdu_bytes, GnTransport::Shb, BtpKind::B, BtpPort::MAPEM)
                        .with_llc_snap(false)
                }
                FrameMsg::Srm => {
                    NetMeta::gn(sdu_bytes, GnTransport::Shb, BtpKind::B, BtpPort::SREM)
                        .with_llc_snap(false)
                }
                FrameMsg::Ssm => {
                    NetMeta::gn(sdu_bytes, GnTransport::Shb, BtpKind::B, BtpPort::SSEM)
                        .with_llc_snap(false)
                }
            },
        }
    }

    /// The network and transport header this stack puts in front of an SDU, octets,
    /// LLC/SNAP excluded.
    pub const fn header_bytes(&self, msg: FrameMsg, sdu_bytes: u32) -> u32 {
        let meta = self.meta(msg, sdu_bytes);
        match self {
            NetStack::Wsmp(w) => w.header_size(&meta),
            NetStack::GnBtp(g) => g.header_size(&meta),
        }
    }
}

/// The shapes of message the network layer distinguishes: which PSID a WSM carries, and
/// which BTP port and GeoNetworking transport a GN packet takes.
///
/// | Message | WSMP PSID | GN transport, BTP-B port |
/// |---|---|---|
/// | BSM / CAM ([`FrameMsg::Safety`]) | `0x20` | SHB, 2001 |
/// | DENM | `0x20` | GBC, 2002 |
/// | SPaT / SPATEM | `0x82` | SHB, 2004 |
/// | MAP / MAPEM | `0x82` | SHB, 2003 |
/// | SRM / SREM | `0x82` | SHB, 2007 |
/// | SSM / SSEM | `0x82` | SHB, 2008 |
///
/// The BTP ports are TS 103 301's `CSP_PortNo` (VERIFIED for 2003, 2004 and 2007; 2008
/// through 04-models.md §7.2, UNVERIFIED). The WSMP PSID of the intersection messages is
/// [`Psid::INTERSECTION`], recalled and unverified — see there. That the intersection
/// messages go single-hop is the common deployment (an RSU broadcasting to its own
/// approaches) and is this build's choice for all four; a GeoBroadcast SPATEM would add
/// the GBC header's extra octets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMsg {
    /// A periodic safety message (BSM, CAM) or anything else sent single-hop.
    Safety,
    /// A DENM, which GeoNetworking sends as GeoBroadcast.
    Denm,
    /// Signal phase and timing (J2735 SPaT, or the ETSI SPATEM that wraps it).
    Spat,
    /// Intersection geometry (J2735 MapData, or the ETSI MAPEM that wraps it).
    Map,
    /// A signal request (J2735 SRM, ETSI SREM).
    Srm,
    /// A signal request's status (J2735 SSM, ETSI SSEM).
    Ssm,
}

/// Every octet of one frame's PSDU, by the layer it belongs to.
///
/// The fields partition the PSDU: [`FrameLayers::psdu_bytes`] is their sum and nothing is
/// counted twice. A frame whose payload and envelope were not built separately — the two
/// the engine sizes from a protocol wire table — carries its whole SPDU in
/// [`FrameLayers::spdu_unsplit`] and zero in the two split fields, so the partition still
/// holds and an overhead ratio computed from `payload` and `security` can see that it has
/// nothing to say about that frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLayers {
    /// The facilities-layer payload: the application's own bytes.
    pub payload: u32,
    /// The security envelope around it: headers, signer identifier, signature.
    pub security: u32,
    /// An SPDU whose payload/envelope split is not known (a frame sized from a table).
    pub spdu_unsplit: u32,
    /// The fragmentation strategy's own per-fragment header, zero for a whole SDU.
    pub fragmentation: u32,
    /// The network and transport header: WSMP, or GeoNetworking plus BTP.
    pub network: u32,
    /// The LLC/SNAP header.
    pub llc_snap: u32,
    /// The 802.11 MAC header.
    pub mac_header: u32,
    /// The frame check sequence.
    pub fcs: u32,
}

impl FrameLayers {
    /// The layers of one whole (unfragmented) frame on `stack`.
    ///
    /// `payload` and `security` are the node's own split; pass `None` for a frame whose
    /// SPDU was sized rather than built, with its length in `spdu`.
    pub fn compose(stack: &NetStack, msg: FrameMsg, split: Option<(u32, u32)>, spdu: u32) -> Self {
        let (payload, security, spdu_unsplit) = match split {
            Some((p, s)) => (p, s, 0),
            None => (0, 0, spdu),
        };
        FrameLayers {
            payload,
            security,
            spdu_unsplit,
            fragmentation: 0,
            network: stack.header_bytes(msg, spdu),
            llc_snap: LLC_SNAP_BYTES,
            mac_header: MAC_HEADER_QOS_DATA_BYTES,
            fcs: FCS_BYTES,
        }
    }

    /// The layers of one fragment carrying `carried` SDU octets and `frag_header` octets
    /// of fragmentation overhead.
    ///
    /// The SDU's own split is apportioned to the fragment in proportion to the octets it
    /// carries, in integer arithmetic with the remainder on the security side, so that the
    /// fragments of one SDU sum back to the SDU exactly when their `carried` values do.
    /// That is an accounting convention, not a claim about which octets a fragment holds;
    /// the card says so.
    pub fn fragment(
        stack: &NetStack,
        msg: FrameMsg,
        split: Option<(u32, u32)>,
        spdu: u32,
        carried: u32,
        frag_header: u32,
    ) -> Self {
        let mut l = Self::compose(stack, msg, split, carried + frag_header);
        match split {
            Some((p, _)) if spdu > 0 => {
                let share = (u64::from(p) * u64::from(carried) / u64::from(spdu)) as u32;
                l.payload = share;
                l.security = carried - share;
                l.spdu_unsplit = 0;
            }
            _ => {
                l.payload = 0;
                l.security = 0;
                l.spdu_unsplit = carried;
            }
        }
        l.fragmentation = frag_header;
        l
    }

    /// The SPDU octets this frame carries: payload plus envelope, or the unsplit SPDU.
    pub const fn spdu_bytes(&self) -> u32 {
        self.payload + self.security + self.spdu_unsplit
    }

    /// Everything below the SPDU: fragmentation, network, LLC/SNAP, MAC header and FCS.
    pub const fn below_spdu_bytes(&self) -> u32 {
        self.fragmentation + self.network + self.llc_snap + self.mac_header + self.fcs
    }

    /// The link layer's share: LLC/SNAP, MAC header and FCS.
    pub const fn link_bytes(&self) -> u32 {
        self.llc_snap + self.mac_header + self.fcs
    }

    /// The PSDU: the octets the PHY puts on the air, and what air time is computed from.
    pub const fn psdu_bytes(&self) -> u32 {
        self.spdu_bytes() + self.below_spdu_bytes()
    }

    /// The MSDU: everything the MAC carries between its header and its FCS.
    pub const fn msdu_bytes(&self) -> u32 {
        self.psdu_bytes() - self.mac_header - self.fcs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bsm_over_wsmp_carries_43_octets_around_its_spdu() {
        let stack = NetStack::from_name("wsmp").unwrap();
        // A BSM with a digest signer: the 1609.2 envelope is 93 octets on a payload the
        // encoder produced (the payload size here is illustrative).
        let l = FrameLayers::compose(&stack, FrameMsg::Safety, Some((40, 93)), 133);
        assert_eq!(
            l.network, 5,
            "04-models.md §7.1: WSMP carries 5 octets for a BSM"
        );
        assert_eq!(l.link_bytes(), 8 + 26 + 4);
        assert_eq!(l.below_spdu_bytes(), 43);
        assert_eq!(l.psdu_bytes(), 133 + 43);
    }

    #[test]
    fn a_cam_over_gn_btp_carries_the_design_s_52_octets_plus_the_mac() {
        let stack = NetStack::from_name("gn-btp").unwrap();
        let l = FrameLayers::compose(&stack, FrameMsg::Safety, Some((200, 157)), 357);
        // 04-models.md §7.2: "Below a secured CAM | 52 | LLC/SNAP 8 + SHB 40 + BTP-B 4".
        assert_eq!(l.network + l.llc_snap, 52);
        assert_eq!(l.psdu_bytes(), 357 + 52 + 26 + 4);
    }

    #[test]
    fn frame_layers_partition_the_psdu() {
        let stack = NetStack::from_name("wsmp").unwrap();
        for (split, spdu) in [(Some((10, 93)), 103), (None, 250), (Some((0, 0)), 0)] {
            let l = FrameLayers::compose(&stack, FrameMsg::Safety, split, spdu);
            let parts = [
                l.payload,
                l.security,
                l.spdu_unsplit,
                l.fragmentation,
                l.network,
                l.llc_snap,
                l.mac_header,
                l.fcs,
            ];
            assert_eq!(parts.iter().sum::<u32>(), l.psdu_bytes());
            assert_eq!(l.spdu_bytes(), spdu);
        }
    }

    #[test]
    fn fragments_of_one_sdu_sum_back_to_it() {
        let stack = NetStack::from_name("wsmp").unwrap();
        let (payload, security) = (1_000, 1_900);
        let spdu = payload + security;
        let carried = [1_200, 1_200, 500];
        let frags: Vec<FrameLayers> = carried
            .iter()
            .map(|&c| {
                FrameLayers::fragment(
                    &stack,
                    FrameMsg::Safety,
                    Some((payload, security)),
                    spdu,
                    c,
                    4,
                )
            })
            .collect();
        let p: u32 = frags.iter().map(|f| f.payload).sum();
        let s: u32 = frags.iter().map(|f| f.security).sum();
        assert_eq!(p + s, spdu);
        // Integer apportionment loses at most one octet per fragment to the other side.
        assert!(p.abs_diff(payload) <= carried.len() as u32);
        assert!(frags.iter().all(|f| f.fragmentation == 4));
    }

    #[test]
    fn an_unknown_layer_is_refused_not_defaulted() {
        assert!(NetStack::from_name("ipv6").is_none());
    }

    #[test]
    fn the_msdu_excludes_the_mac_header_and_fcs() {
        let stack = NetStack::from_name("gn-btp").unwrap();
        let l = FrameLayers::compose(&stack, FrameMsg::Denm, Some((100, 150)), 250);
        assert_eq!(
            l.network,
            56 + 4,
            "a DENM is GeoBroadcast (56) with BTP-B (4)"
        );
        assert_eq!(l.msdu_bytes(), 250 + 60 + 8);
    }
}
