//! The `NetLayer` seam of 03-interfaces.md §5, and the header-field types both
//! implementations share.
//!
//! A `NetLayer` model answers one question exactly — **how many octets does this layer put
//! in front of the SDU** — and two questions by construction: what the MTU is, and what
//! happens to an SDU above it. 04-models.md §7 is explicit about the last one: neither
//! WSMP nor GeoNetworking has a fragmentation field, so [`NetLayer::encapsulate`] never
//! returns more than one PDU and an oversize SDU is refused
//! ([`crate::error::NetError::Oversize`]). Fragmentation is the [`crate::frag`] seam,
//! which sits above this one.
//!
//! # Sizes, not synthesised octets
//!
//! A [`NetPdu`] carries the SDU's real bytes (they came from a codec) and the header's
//! *size and decoded fields* — not a byte-for-byte GeoNetworking or WSMP header. Nothing
//! in the simulator parses those octets: the PHY needs the frame's length, the receiver
//! needs the transport identifier and the payload, and the metric layer needs the byte
//! count. Synthesising 40 octets of Long Position Vector that no code reads would add a
//! second, unvalidated encoder next to the ASN.1 one in `v2xw-msg` and would still not be
//! interoperable with a real stack. The crate is therefore honest about what it produces,
//! the way `v2xw-msg` is honest about its size-model tier: [`NetPdu::header_bytes`] is
//! exact per the standard's own tables, and there is no claim beyond that.
//!
//! # Why one `NetMeta` for two standards
//!
//! 03-interfaces.md gives both layers the same signature, `header_bytes(&self, meta:
//! &NetMeta) -> u32`, with no `Result`. A meta type that were an enum over the two
//! standards would force every caller that does not yet know which stack a node runs to
//! choose one, and would make the size query fallible for a mismatch that is a
//! configuration error rather than a per-packet event. [`NetMeta`] is therefore flat: it
//! carries the WSMP fields *and* the GeoNetworking/BTP fields, each layer reads the ones
//! its standard defines, and each layer's documentation says which ones it ignores. The
//! constructors ([`NetMeta::wsmp`], [`NetMeta::gn`]) fill the other stack's fields with
//! their documented defaults, so a meta built for one layer is never half-initialised.

use serde::{Deserialize, Serialize};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;

use crate::error::{DropCause, NetError, Result};

// =========================================================================================
// IEEE 1609.3 p-encoding (VarLengthNumber)
// =========================================================================================

/// The largest value the IEEE 1609.3 p-encoding represents: `0x1020407F`, 270,549,119.
///
/// [IEEE PSID tutorial via R4 §F.1; 04-models.md §7.1, VERIFIED]
pub const VAR_LENGTH_NUMBER_MAX: u32 = 0x1020_407F;

/// Largest value in one octet: prefix `0`, 7 value bits.
const P_ENCODED_1_MAX: u32 = 0x7F;
/// Largest value in two octets: prefix `10`, 14 value bits, offset `0x80`.
const P_ENCODED_2_MAX: u32 = 0x407F;
/// Largest value in three octets: prefix `110`, 21 value bits, offset `0x4080`.
const P_ENCODED_3_MAX: u32 = 0x20_407F;

/// How many octets the IEEE 1609.3 p-encoding needs for `value`, or `None` above
/// [`VAR_LENGTH_NUMBER_MAX`].
///
/// The encoding prefixes the first octet with as many `1` bits as there are extra octets
/// and adds the previous range's exclusive end as an offset, which gives the boundaries:
///
/// | Octets | Prefix | Value bits | Range |
/// |---|---|---|---|
/// | 1 | `0` | 7 | `0 ..= 0x7F` |
/// | 2 | `10` | 14 | `0x80 ..= 0x407F` |
/// | 3 | `110` | 21 | `0x4080 ..= 0x20407F` |
/// | 4 | `1110` | 28 | `0x204080 ..= 0x1020407F` |
///
/// The top of the four-octet range is the maximum the IEEE PSID tutorial quotes for a PSID,
/// `0x1020407F` — which is the arithmetic check that these offsets are the standard's own
/// [R4 §F.1].
///
/// ```
/// use v2xw_net::netlayer::p_encoded_bytes;
/// assert_eq!(p_encoded_bytes(0x20), Some(1));        // the BSM PSID
/// assert_eq!(p_encoded_bytes(0x7F), Some(1));
/// assert_eq!(p_encoded_bytes(0x80), Some(2));
/// assert_eq!(p_encoded_bytes(0x1020_407F), Some(4)); // the largest PSID
/// assert_eq!(p_encoded_bytes(0x1020_4080), None);
/// ```
pub const fn p_encoded_bytes(value: u32) -> Option<u32> {
    if value <= P_ENCODED_1_MAX {
        Some(1)
    } else if value <= P_ENCODED_2_MAX {
        Some(2)
    } else if value <= P_ENCODED_3_MAX {
        Some(3)
    } else if value <= VAR_LENGTH_NUMBER_MAX {
        Some(4)
    } else {
        None
    }
}

/// A Provider Service Identifier: the WSMP-T destination address, p-encoded in one to four
/// octets (04-models.md §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Psid(u32);

impl Psid {
    /// The PSID of the SAE J2735 Basic Safety Message, `0x20`.
    ///
    /// The value 04-models.md §7.1 and §9.1 both use — §9.1 sizes the envelope's `psid`
    /// field at 2 octets "PSID 0x20" — and the one the default BSM header of 5 octets is
    /// derived from. One p-encoded octet.
    pub const BSM: Psid = Psid(0x20);

    /// A PSID from its numeric value.
    ///
    /// # Errors
    /// [`NetError::PsidOutOfRange`] above [`VAR_LENGTH_NUMBER_MAX`], which the p-encoding
    /// cannot represent.
    pub const fn new(value: u32) -> Result<Self> {
        if value > VAR_LENGTH_NUMBER_MAX {
            return Err(NetError::PsidOutOfRange {
                psid: value,
                max: VAR_LENGTH_NUMBER_MAX,
            });
        }
        Ok(Psid(value))
    }

    /// The numeric value.
    pub const fn value(self) -> u32 {
        self.0
    }

    /// How many octets this PSID occupies on the wire, 1 to 4.
    pub const fn encoded_bytes(self) -> u32 {
        // Constructed values are in range, so the option is always `Some`; `unwrap` is not
        // const-stable for `Option<u32>` in this position, hence the match.
        match p_encoded_bytes(self.0) {
            Some(n) => n,
            None => 4,
        }
    }
}

impl core::fmt::Display for Psid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

// =========================================================================================
// WSMP N-header extensions
// =========================================================================================

/// Octets one WSMP N-header extension occupies: element id 1 + length 1 + a one-octet
/// value (04-models.md §7.1) [Wireshark `packet-wsmp.c`; IEEE 1609.3 `wee.asn`].
pub const WSMP_N_EXTENSION_BYTES: u32 = 3;

/// Which of the three optional WSMP N-header extensions are present.
///
/// All three carry a one-octet value — `ChannelNumber80211 INTEGER(0..255)`,
/// `DataRate80211 INTEGER(0..255)`, `TXpower80211 INTEGER(-128..127)` — so each costs
/// [`WSMP_N_EXTENSION_BYTES`] octets: element id, length, value (04-models.md §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsmpExtensions {
    /// Channel number (element id 15).
    pub channel_number: bool,
    /// Data rate (element id 16).
    pub data_rate: bool,
    /// Transmit power (element id 4).
    pub transmit_power: bool,
}

impl WsmpExtensions {
    /// No extensions: the default, and what a BSM sends.
    pub const NONE: WsmpExtensions = WsmpExtensions {
        channel_number: false,
        data_rate: false,
        transmit_power: false,
    };

    /// All three extensions — the configuration 04-models.md §7.1 derives 14 octets for.
    pub const ALL: WsmpExtensions = WsmpExtensions {
        channel_number: true,
        data_rate: true,
        transmit_power: true,
    };

    /// How many extensions are present, 0 to 3.
    pub const fn count(self) -> u32 {
        self.channel_number as u32 + self.data_rate as u32 + self.transmit_power as u32
    }

    /// Octets the present extensions occupy.
    pub const fn bytes(self) -> u32 {
        self.count() * WSMP_N_EXTENSION_BYTES
    }
}

// =========================================================================================
// GeoNetworking and BTP header shapes
// =========================================================================================

/// GeoNetworking Basic Header, octets [EN 302 636-4-1 V1.4.1 §9.6, Tables 11-17, VERIFIED].
pub const GN_BASIC_HEADER_BYTES: u32 = 4;
/// GeoNetworking Common Header, octets [EN 302 636-4-1 V1.4.1 §9.7, Tables 11-17, VERIFIED].
pub const GN_COMMON_HEADER_BYTES: u32 = 8;
/// Long Position Vector, octets [EN 302 636-4-1 V1.4.1 §9.5.2, Tables 11-17, VERIFIED].
pub const GN_LONG_POSITION_VECTOR_BYTES: u32 = 24;
/// Short Position Vector, octets [EN 302 636-4-1 V1.4.1 §9.5.3, Table 11, VERIFIED].
pub const GN_SHORT_POSITION_VECTOR_BYTES: u32 = 20;
/// Sequence number field, octets [Tables 11-15].
const GN_SEQUENCE_NUMBER_BYTES: u32 = 2;
/// A reserved field, octets [Tables 11-15].
const GN_RESERVED_BYTES: u32 = 2;
/// SHB media-dependent data, octets 36-39 of the SHB header [Table 13].
const GN_SHB_MEDIA_DEPENDENT_BYTES: u32 = 4;
/// Latitude or longitude of a geo-area, octets [Table 14].
const GN_GEO_COORD_BYTES: u32 = 4;
/// A geo-area distance (a or b), octets [Table 14].
const GN_GEO_DISTANCE_BYTES: u32 = 2;
/// The geo-area angle, octets [Table 14].
const GN_GEO_ANGLE_BYTES: u32 = 2;

/// A BTP header, octets: destination port 2 + source port or destination port info 2
/// [EN 302 636-5-1 V2.2.1 §7.2-7.3, Tables 2-3, VERIFIED].
pub const BTP_HEADER_BYTES: u32 = 4;

/// IEEE 802.2 LLC plus SNAP over ITS-G5, octets: DSAP/SSAP 0xAA, control 0x03,
/// OUI 00-00-00, EtherType.
///
/// DERIVED from 802.2/SNAP; EN 302 663 §4.3.1 Figure 3 shows "IEEE/ISO/IEC 8802-2 with
/// SNAP" but the octet count is not quoted in an accessible clause
/// (04-models.md §4.6, §7.2; R4 §F.2, status DERIVED).
pub const LLC_SNAP_BYTES: u32 = 8;

/// EtherType of GeoNetworking [Wireshark `epan/etypes.h`, secondary].
pub const ETHERTYPE_GEONETWORKING: u16 = 0x8947;
/// EtherType of WSMP [Wireshark `epan/etypes.h`, secondary].
pub const ETHERTYPE_WSMP: u16 = 0x88DC;

/// The GeoNetworking transport type of a packet — the shape of its header.
///
/// [`GnTransport::header_bytes`] builds each total from the fields EN 302 636-4-1's tables
/// list, so the code *is* the composition column of 04-models.md §7.2 and the tests pin the
/// documented totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GnTransport {
    /// Beacon: Basic + Common + Long Position Vector [Table 15].
    Beacon,
    /// Single-hop broadcast — CAM, CPM, VAM [Table 13].
    Shb,
    /// Topologically scoped broadcast [Table 12].
    Tsb,
    /// GeoBroadcast — DENM [Table 14].
    Gbc,
    /// GeoAnycast; same header as [`GnTransport::Gbc`] [Table 14].
    Gac,
    /// GeoUnicast: the largest header, which is what `itsGnMaxGeoNetworkingHeaderSize`
    /// accounts for [Table 11].
    Guc,
}

impl GnTransport {
    /// Every transport type, in declaration order.
    pub const ALL: [GnTransport; 6] = [
        GnTransport::Beacon,
        GnTransport::Shb,
        GnTransport::Tsb,
        GnTransport::Gbc,
        GnTransport::Gac,
        GnTransport::Guc,
    ];

    /// The GeoNetworking header size for this transport type, octets.
    ///
    /// Composed from the field sizes of EN 302 636-4-1's Tables 11-15, which is how
    /// 04-models.md §7.2 states them:
    ///
    /// | Type | Composition | Octets |
    /// |---|---|---|
    /// | BEACON | 4 + 8 + LPV 24 | 36 |
    /// | SHB | 4 + 8 + SO PV 24 + media-dependent 4 | 40 |
    /// | TSB | 4 + 8 + SN 2 + reserved 2 + LPV 24 | 40 |
    /// | GBC / GAC | 4 + 8 + SN 2 + reserved 2 + LPV 24 + lat 4 + long 4 + dist-a 2 + dist-b 2 + angle 2 + reserved 2 | 56 |
    /// | GUC | 4 + 8 + SN 2 + reserved 2 + LPV 24 + SPV 20 | 60 |
    pub const fn header_bytes(self) -> u32 {
        let common = GN_BASIC_HEADER_BYTES + GN_COMMON_HEADER_BYTES;
        match self {
            GnTransport::Beacon => common + GN_LONG_POSITION_VECTOR_BYTES,
            GnTransport::Shb => {
                common + GN_LONG_POSITION_VECTOR_BYTES + GN_SHB_MEDIA_DEPENDENT_BYTES
            }
            GnTransport::Tsb => {
                common
                    + GN_SEQUENCE_NUMBER_BYTES
                    + GN_RESERVED_BYTES
                    + GN_LONG_POSITION_VECTOR_BYTES
            }
            GnTransport::Gbc | GnTransport::Gac => {
                common
                    + GN_SEQUENCE_NUMBER_BYTES
                    + GN_RESERVED_BYTES
                    + GN_LONG_POSITION_VECTOR_BYTES
                    + 2 * GN_GEO_COORD_BYTES
                    + 2 * GN_GEO_DISTANCE_BYTES
                    + GN_GEO_ANGLE_BYTES
                    + GN_RESERVED_BYTES
            }
            GnTransport::Guc => {
                common
                    + GN_SEQUENCE_NUMBER_BYTES
                    + GN_RESERVED_BYTES
                    + GN_LONG_POSITION_VECTOR_BYTES
                    + GN_SHORT_POSITION_VECTOR_BYTES
            }
        }
    }

    /// The name this type serialises and reports as.
    pub const fn as_str(self) -> &'static str {
        match self {
            GnTransport::Beacon => "beacon",
            GnTransport::Shb => "shb",
            GnTransport::Tsb => "tsb",
            GnTransport::Gbc => "gbc",
            GnTransport::Gac => "gac",
            GnTransport::Guc => "guc",
        }
    }
}

impl core::fmt::Display for GnTransport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which BTP header a packet carries [EN 302 636-5-1 V2.2.1 §7.2-7.3].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BtpKind {
    /// BTP-A, interactive (next-header 1): destination port 2 + source port 2.
    A,
    /// BTP-B, non-interactive (next-header 2): destination port 2 + destination port info
    /// 2. The default for the broadcast message set.
    B,
}

impl BtpKind {
    /// The BTP header size, octets. Both forms are [`BTP_HEADER_BYTES`].
    pub const fn header_bytes(self) -> u32 {
        BTP_HEADER_BYTES
    }

    /// The name this kind serialises and reports as.
    pub const fn as_str(self) -> &'static str {
        match self {
            BtpKind::A => "btp-a",
            BtpKind::B => "btp-b",
        }
    }
}

impl core::fmt::Display for BtpKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A BTP destination port.
///
/// The constants carry their status: the five from TS 103 301's `CSP_PortNo` tables are
/// VERIFIED; CAM, DENM and SSEM are attributed to TS 103 248, which was not extracted, and
/// are marked UNVERIFIED in 04-models.md §7.2. Port numbers do not change any header size —
/// a BTP header is four octets whatever the port — so an unverified port affects
/// demultiplexing in the model and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BtpPort(pub u16);

impl BtpPort {
    /// CAM, port 2001 [TS 103 248 via 04-models.md §7.2, UNVERIFIED].
    pub const CAM: BtpPort = BtpPort(2001);
    /// DENM, port 2002 [TS 103 248 via 04-models.md §7.2, UNVERIFIED].
    pub const DENM: BtpPort = BtpPort(2002);
    /// MAPEM, port 2003 [TS 103 301 `CSP_PortNo`, VERIFIED].
    pub const MAPEM: BtpPort = BtpPort(2003);
    /// SPATEM, port 2004 [TS 103 301 `CSP_PortNo`, VERIFIED].
    pub const SPATEM: BtpPort = BtpPort(2004);
    /// IVIM, port 2006 [TS 103 301 `CSP_PortNo`, VERIFIED].
    pub const IVIM: BtpPort = BtpPort(2006);
    /// SREM, port 2007 [TS 103 301 `CSP_PortNo`, VERIFIED].
    pub const SREM: BtpPort = BtpPort(2007);
    /// SSEM, port 2008 [TS 103 248 via 04-models.md §7.2, UNVERIFIED].
    pub const SSEM: BtpPort = BtpPort(2008);
    /// RTCMEM, port 2013 [TS 103 301 `CSP_PortNo`, VERIFIED].
    pub const RTCMEM: BtpPort = BtpPort(2013);

    /// The port number.
    pub const fn value(self) -> u16 {
        self.0
    }
}

impl core::fmt::Display for BtpPort {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// =========================================================================================
// The seam
// =========================================================================================

/// What a network layer needs in order to size its header.
///
/// Flat across both standards on purpose; see the module documentation. Build one with
/// [`NetMeta::wsmp`] or [`NetMeta::gn`] (or the per-message shorthands), which fill the
/// other stack's fields with their documented defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetMeta {
    /// The SDU handed down from the facilities layer, bytes. For a V2X safety message this
    /// is the secured PDU (the message plus its IEEE 1609.2 / TS 103 097 envelope), because
    /// the envelope is what the network layer carries.
    pub sdu_bytes: u32,
    /// WSMP: the destination PSID.
    pub psid: Psid,
    /// WSMP: which optional N-header extensions are present.
    pub extensions: WsmpExtensions,
    /// GeoNetworking: the packet transport type, which fixes the GN header's shape.
    pub gn: GnTransport,
    /// BTP: which of the two transport headers is used.
    pub btp: BtpKind,
    /// BTP: the destination port.
    pub port: BtpPort,
    /// Whether the LLC/SNAP octets below the network layer are counted here.
    ///
    /// `true` for GeoNetworking over ITS-G5, because the 52-octet figure 04-models.md §7.2
    /// derives for "below a secured CAM" includes them; `false` for WSMP, because the
    /// 5-octet BSM figure of §7.1 does not. Invariant I-N1 is the reason this is a flag and
    /// not a constant: whichever layer counts the eight octets, only one of them may, and
    /// the MAC model's own frame overhead (04-models.md §4.6) is where they go when this is
    /// `false`.
    pub include_llc_snap: bool,
}

impl NetMeta {
    /// A meta for the WSMP stack: a PSID, no N-header extensions, LLC/SNAP counted by the
    /// MAC.
    pub const fn wsmp(sdu_bytes: u32, psid: Psid) -> Self {
        Self {
            sdu_bytes,
            psid,
            extensions: WsmpExtensions::NONE,
            // The GeoNetworking fields are unread by the WSMP layer; these are the
            // broadcast defaults, so a meta is never half-initialised.
            gn: GnTransport::Shb,
            btp: BtpKind::B,
            port: BtpPort::CAM,
            include_llc_snap: false,
        }
    }

    /// A meta for the GeoNetworking/BTP stack over ITS-G5, with LLC/SNAP counted here.
    pub const fn gn(sdu_bytes: u32, gn: GnTransport, btp: BtpKind, port: BtpPort) -> Self {
        Self {
            sdu_bytes,
            // Unread by the GeoNetworking layer.
            psid: Psid::BSM,
            extensions: WsmpExtensions::NONE,
            gn,
            btp,
            port,
            include_llc_snap: true,
        }
    }

    /// A BSM over WSMP: PSID `0x20`, no extensions (04-models.md §7.1).
    pub const fn for_bsm(sdu_bytes: u32) -> Self {
        Self::wsmp(sdu_bytes, Psid::BSM)
    }

    /// A CAM over GeoNetworking/BTP-B: single-hop broadcast, port 2001 (04-models.md §7.2).
    pub const fn for_cam(sdu_bytes: u32) -> Self {
        Self::gn(sdu_bytes, GnTransport::Shb, BtpKind::B, BtpPort::CAM)
    }

    /// A DENM over GeoNetworking/BTP-B: GeoBroadcast, port 2002 (04-models.md §7.2).
    pub const fn for_denm(sdu_bytes: u32) -> Self {
        Self::gn(sdu_bytes, GnTransport::Gbc, BtpKind::B, BtpPort::DENM)
    }

    /// The same meta with the given N-header extensions.
    #[must_use]
    pub const fn with_extensions(mut self, extensions: WsmpExtensions) -> Self {
        self.extensions = extensions;
        self
    }

    /// The same meta with [`NetMeta::include_llc_snap`] set.
    #[must_use]
    pub const fn with_llc_snap(mut self, include: bool) -> Self {
        self.include_llc_snap = include;
        self
    }

    /// The same meta for a different SDU size.
    #[must_use]
    pub const fn with_sdu_bytes(mut self, sdu_bytes: u32) -> Self {
        self.sdu_bytes = sdu_bytes;
        self
    }
}

/// How a PDU identifies the service it is destined for.
///
/// Unlike [`NetMeta`] this *is* an enum: a PDU was produced by one specific layer, so
/// exactly one of the two forms applies, and a receiver that is handed the wrong one has
/// been given a foreign PDU ([`DropCause::MalformedPdu`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// WSMP: a p-encoded PSID.
    Wsmp {
        /// The destination PSID.
        psid: Psid,
    },
    /// BTP over GeoNetworking: a header kind and a destination port.
    Btp {
        /// BTP-A or BTP-B.
        kind: BtpKind,
        /// The destination port.
        port: BtpPort,
    },
}

/// One network-layer protocol data unit: a header size, its decoded transport identifier
/// and the SDU it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetPdu {
    /// The model id of the layer that produced it, e.g. `net/wsmp/1609-3`.
    pub layer: &'static str,
    /// The exact header size this layer put in front of the SDU, octets — including the
    /// LLC/SNAP octets when [`NetMeta::include_llc_snap`] asked for them.
    pub header_bytes: u32,
    /// Which service the PDU is destined for.
    pub transport: Transport,
    /// The GeoNetworking transport type, for a PDU from the GeoNetworking layer.
    pub gn: Option<GnTransport>,
    /// The SDU's bytes, as the facilities layer produced them.
    pub payload: Vec<u8>,
}

impl NetPdu {
    /// The SDU's size, bytes.
    pub fn sdu_bytes(&self) -> u32 {
        // An SDU is at most a few thousand bytes; the cast cannot lose information for any
        // payload this crate accepts, and `encapsulate` checks the MTU first.
        u32::try_from(self.payload.len()).unwrap_or(u32::MAX)
    }

    /// Header plus SDU, bytes: what the layer below is asked to carry.
    pub fn total_bytes(&self) -> u32 {
        self.header_bytes.saturating_add(self.sdu_bytes())
    }
}

/// What a receiving network layer did with a PDU.
///
/// 03-interfaces.md §5 lists a third outcome, `Forward(...)`: GeoNetworking's multi-hop
/// GBC forwarding and DENM keep-alive forwarding. 04-models.md §7.2 and §7.5 put both in
/// the `high` net tier, which this crate does not implement, so the variant is not declared
/// — a variant nothing produces invites a match arm that is never taken and looks tested.
/// The enum is `#[non_exhaustive]`, so adding it with the forwarding model is not a
/// breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecapOutcome {
    /// The PDU was for this node: here is the SDU and the service it belongs to.
    Deliver {
        /// The SDU's bytes.
        sdu: Vec<u8>,
        /// Which service to hand it to.
        transport: Transport,
    },
    /// The PDU was discarded, for exactly one reason.
    Drop(DropCause),
}

/// A network-layer model: WSMP (IEEE 1609.3) or GeoNetworking + BTP (EN 302 636)
/// (03-interfaces.md §5, 04-models.md §7).
///
/// # Why the trait is generic over the context
///
/// [`v2xw_core::ctx::Ctx`] carries associated types (`World`, `Actors`, `Payload`), so
/// `&mut dyn Ctx` on its own does not name a type, and this crate sits below the engine
/// crate that fixes them. The trait therefore takes the context as a type parameter; once
/// the engine fixes it, `dyn NetLayer<EngineCtx>` is an ordinary trait object, which is how
/// in-process plug-ins are called (ADR 0007 §8). This is the shape `v2xw-msg`'s
/// `MessageGenerator` already uses.
pub trait NetLayer<C>: Model
where
    C: Ctx + ?Sized,
{
    /// The exact number of header octets this layer adds for `meta` (04-models.md §7).
    fn header_bytes(&self, meta: &NetMeta) -> u32;

    /// Wraps `sdu` in this layer's header.
    ///
    /// Returns a `Vec` because the signature is shared with layers that might fragment;
    /// neither of the two modelled standards does, so the result always holds exactly one
    /// PDU — [`NetLayer::fragments`] says so, and the tests pin it.
    ///
    /// # Errors
    /// [`NetError::Oversize`] when the SDU exceeds [`NetLayer::mtu`], because there is no
    /// fragmentation field to split it with. A [`crate::frag::Fragmenter`] must act first.
    fn encapsulate(&self, sdu: &[u8], meta: &NetMeta) -> Result<Vec<NetPdu>>;

    /// Receives a PDU: deliver its SDU, or drop it with a cause.
    ///
    /// Takes the context because the `high` tier's forwarding has to schedule the
    /// re-broadcast as an [`v2xw_core::event::EventClass::NetDeliver`] event (invariant
    /// I-N3). The two implementations in this crate use it for nothing — they name it
    /// `_ctx` — and keeping it in the signature is what lets the forwarding tier land
    /// without changing every caller.
    fn decapsulate(&mut self, ctx: &mut C, rx: NodeId, pdu: &NetPdu) -> DecapOutcome;

    /// The largest SDU this layer accepts, bytes.
    fn mtu(&self) -> u32;

    /// Whether this layer fragments. Always `false` for both modelled standards, which is
    /// why it is defaulted: WSMP's `ShortMsgNpdu` and GeoNetworking's headers have no
    /// fragmentation field at all (04-models.md §7.1, §7.2; R4 §F.1-F.2, VERIFIED by
    /// absence).
    fn fragments(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The p-encoding boundaries are the standard's own: each range starts one past the
    /// previous one's end, and the four-octet range ends at the PSID maximum the IEEE
    /// tutorial quotes.
    #[test]
    fn the_p_encoding_boundaries_reproduce_the_documented_maximum() {
        assert_eq!(p_encoded_bytes(0), Some(1));
        assert_eq!(p_encoded_bytes(P_ENCODED_1_MAX), Some(1));
        assert_eq!(p_encoded_bytes(P_ENCODED_1_MAX + 1), Some(2));
        assert_eq!(p_encoded_bytes(P_ENCODED_2_MAX), Some(2));
        assert_eq!(p_encoded_bytes(P_ENCODED_2_MAX + 1), Some(3));
        assert_eq!(p_encoded_bytes(P_ENCODED_3_MAX), Some(3));
        assert_eq!(p_encoded_bytes(P_ENCODED_3_MAX + 1), Some(4));
        assert_eq!(p_encoded_bytes(VAR_LENGTH_NUMBER_MAX), Some(4));
        assert_eq!(p_encoded_bytes(VAR_LENGTH_NUMBER_MAX + 1), None);

        // The offsets are the previous range's exclusive end, which is what makes the
        // four-octet top 0x1020407F = 270,549,119.
        assert_eq!(P_ENCODED_1_MAX + 1 + ((1 << 14) - 1), P_ENCODED_2_MAX);
        assert_eq!(P_ENCODED_2_MAX + 1 + ((1 << 21) - 1), P_ENCODED_3_MAX);
        assert_eq!(
            P_ENCODED_3_MAX + 1 + ((1u32 << 28) - 1),
            VAR_LENGTH_NUMBER_MAX
        );
        assert_eq!(VAR_LENGTH_NUMBER_MAX, 270_549_119);
    }

    #[test]
    fn a_psid_out_of_range_is_refused() {
        assert_eq!(Psid::BSM.encoded_bytes(), 1);
        assert_eq!(Psid::new(0x20).unwrap(), Psid::BSM);
        assert_eq!(Psid::new(0x4080).unwrap().encoded_bytes(), 3);
        assert_eq!(
            Psid::new(VAR_LENGTH_NUMBER_MAX + 1),
            Err(NetError::PsidOutOfRange {
                psid: VAR_LENGTH_NUMBER_MAX + 1,
                max: VAR_LENGTH_NUMBER_MAX
            })
        );
    }

    /// Every GeoNetworking header size of 04-models.md §7.2, built from its own
    /// composition and checked against the documented total.
    #[test]
    fn every_gn_header_size_matches_the_documented_value() {
        let documented = [
            (GnTransport::Beacon, 36u32),
            (GnTransport::Shb, 40),
            (GnTransport::Tsb, 40),
            (GnTransport::Gbc, 56),
            (GnTransport::Gac, 56),
            (GnTransport::Guc, 60),
        ];
        for (t, bytes) in documented {
            assert_eq!(t.header_bytes(), bytes, "{t} header");
        }
        // The building blocks, each with its own clause.
        assert_eq!(GN_BASIC_HEADER_BYTES, 4);
        assert_eq!(GN_COMMON_HEADER_BYTES, 8);
        assert_eq!(GN_LONG_POSITION_VECTOR_BYTES, 24);
        assert_eq!(GN_SHORT_POSITION_VECTOR_BYTES, 20);
        assert_eq!(BTP_HEADER_BYTES, 4);
        assert_eq!(BtpKind::A.header_bytes(), 4);
        assert_eq!(BtpKind::B.header_bytes(), 4);
        assert_eq!(LLC_SNAP_BYTES, 8);
        // GUC is the largest, which is what itsGnMaxGeoNetworkingHeaderSize accounts for.
        assert_eq!(
            GnTransport::ALL
                .iter()
                .map(|t| t.header_bytes())
                .max()
                .unwrap(),
            GnTransport::Guc.header_bytes()
        );
    }

    #[test]
    fn extensions_cost_three_octets_each() {
        assert_eq!(WsmpExtensions::NONE.count(), 0);
        assert_eq!(WsmpExtensions::NONE.bytes(), 0);
        assert_eq!(WsmpExtensions::ALL.count(), 3);
        assert_eq!(WsmpExtensions::ALL.bytes(), 9);
        let one = WsmpExtensions {
            data_rate: true,
            ..WsmpExtensions::NONE
        };
        assert_eq!(one.bytes(), 3);
    }

    #[test]
    fn well_known_ports_keep_their_numbers() {
        assert_eq!(BtpPort::MAPEM.value(), 2003);
        assert_eq!(BtpPort::SPATEM.value(), 2004);
        assert_eq!(BtpPort::IVIM.value(), 2006);
        assert_eq!(BtpPort::SREM.value(), 2007);
        assert_eq!(BtpPort::RTCMEM.value(), 2013);
        assert_eq!(BtpPort::CAM.value(), 2001);
        assert_eq!(BtpPort::DENM.value(), 2002);
    }

    #[test]
    fn the_shorthand_metas_carry_the_documented_defaults() {
        let bsm = NetMeta::for_bsm(180);
        assert_eq!(bsm.psid, Psid::BSM);
        assert_eq!(bsm.extensions, WsmpExtensions::NONE);
        assert!(!bsm.include_llc_snap, "WSMP's 5 octets exclude LLC/SNAP");

        let cam = NetMeta::for_cam(357);
        assert_eq!(cam.gn, GnTransport::Shb);
        assert_eq!(cam.btp, BtpKind::B);
        assert_eq!(cam.port, BtpPort::CAM);
        assert!(cam.include_llc_snap, "the 52-octet figure includes them");

        let denm = NetMeta::for_denm(300);
        assert_eq!(denm.gn, GnTransport::Gbc);
        assert_eq!(denm.port, BtpPort::DENM);
    }
}
