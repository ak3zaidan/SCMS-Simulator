//! `v2xw-net` — the network and transport layer: header sizes, fragmentation strategies,
//! loss amplification, and the byte accounting everything else adds up.
//!
//! # Read this first: this crate counts octets, it does not synthesise them
//!
//! A `NetLayer` model's whole job is the number 04-models.md §7 gives it —
//! "return exact header sizes (`header_bytes`), never fragment (neither WSMP nor
//! GeoNetworking has a fragmentation field), and expose the MTU". A [`netlayer::NetPdu`]
//! therefore carries the SDU's **real bytes** (they came from a codec in `v2xw-msg`) and the
//! header's **exact size and decoded fields** — not a byte-for-byte WSMP or GeoNetworking
//! header. Nothing in the simulator parses those octets: the PHY needs the frame's length,
//! the receiver needs the transport identifier and the payload, and the metric layer needs
//! the byte count. Synthesising 40 octets of Long Position Vector that no code reads would
//! put a second, unvalidated encoder next to the ASN.1 one and would still not interoperate
//! with a real stack. This crate is as explicit about that as `v2xw-msg` is about its
//! size-model tier.
//!
//! Every size here comes from a standard's own table and carries its clause; the ones that
//! do not are marked DERIVED on the model card, with the anchors they were derived from.
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | The `NetLayer` seam, `NetMeta`, `NetPdu`, p-encoding, GN/BTP field sizes | [`netlayer`] | 03-interfaces.md §5, 04-models.md §7 |
//! | `net/wsmp/1609-3` — the IEEE 1609.3 WSMP header | [`wsmp`] | 04-models.md §7.1 |
//! | `net/gn-btp/en302636` — GeoNetworking and BTP headers, `itsGnMaxSduSize` | [`gn`] | 04-models.md §7.2 |
//! | The `Fragmenter` seam, fragment descriptors, the reassembly buffer, the `net.frag` record | [`frag`] | 03-interfaces.md §5, §14; 04-models.md §7.3 |
//! | `fragmenter/none` | [`frag::none`] | 04-models.md §7.3 |
//! | `fragmenter/facilities-segmentation` | [`frag::facilities`] | 04-models.md §7.3 |
//! | `fragmenter/cert-cycle-partial-hybrid` | [`frag::cert_cycle`] | 04-models.md §7.3, NDSS 2024 |
//! | `fragmenter/generic-sdu` | [`frag::generic`] | 04-models.md §7.3 |
//! | `P_sdu = 1 - prod(1 - p_i)` and the hook that measures it | [`amplification`] | 04-models.md §7.4 |
//! | Byte accounting, invariant I-N1 | [`accounting`] | 03-interfaces.md §5 |
//! | One frame's PSDU by layer: payload, envelope, network, LLC/SNAP, MAC, FCS | [`frame`] | 04-models.md §4.6, §7, §9.3 |
//! | Errors and drop causes | [`error`] | — |
//!
//! # The three numbers worth knowing
//!
//! ```
//! use v2xw_net::gn::{BELOW_SECURED_CAM_BYTES, GnBtpNetLayer, ITS_GN_MAX_SDU_SIZE};
//! use v2xw_net::netlayer::NetMeta;
//! use v2xw_net::wsmp::{WSMP_BSM_HEADER_BYTES, WsmpNetLayer};
//!
//! // A BSM over WSMP carries 5 octets of network header (04-models.md §7.1).
//! assert_eq!(WsmpNetLayer::default().header_size(&NetMeta::for_bsm(180)), 5);
//! assert_eq!(WSMP_BSM_HEADER_BYTES, 5);
//!
//! // A secured CAM over ITS-G5 carries 52: LLC/SNAP 8 + GN SHB 40 + BTP-B 4 (§7.2).
//! let gn = GnBtpNetLayer::default();
//! assert_eq!(gn.header_size(&NetMeta::for_cam(357)), BELOW_SECURED_CAM_BYTES);
//! assert_eq!(BELOW_SECURED_CAM_BYTES, 52);
//!
//! // And GeoNetworking's MTU is Annex H's own calculation, 1 500 - 88 - 0.
//! assert_eq!(gn.sdu_mtu(), ITS_GN_MAX_SDU_SIZE);
//! assert_eq!(ITS_GN_MAX_SDU_SIZE, 1_398);
//! ```
//!
//! # Fragmentation happens above the network layer, or not at all
//!
//! Neither standard can split a packet: R4 §F.1 finds no fragmentation field in
//! `ShortMsgNpdu`, and the full text of EN 302 636-4-1 V1.4.1 contains zero occurrences of
//! "fragment" (both VERIFIED by absence). So an oversize SDU has exactly three fates, and
//! [`frag`] is where the choice between them is made: refused ([`frag::none`]), segmented by
//! the facilities layer into independently interpretable pieces
//! ([`frag::facilities`]), or split into pieces that are useless alone
//! ([`frag::generic`], [`frag::cert_cycle`]) — which is the case
//! `P_sdu = 1 - prod(1 - p_i)` is about.
//!
//! # Determinism
//!
//! * **No standard-library transcendental.** Nothing here needs one: header sizes are
//!   integer arithmetic, and the amplification formula is multiplication and subtraction.
//!   `round` and `clamp` are IEEE-754 exact and are used directly (ADR 0003).
//! * **No RNG.** Every model's card declares `uses_rng: false`, and the one rule that needs
//!   randomness — the P2PCD response backoff — takes its uniform draws as arguments
//!   ([`frag::cert_cycle::CertCyclePartialHybrid::learning_response_delay`]), so the draw
//!   stays on the caller's own per-entity stream (ADR 0004 §3).
//! * **No `HashMap`.** The reassembly buffer is a `BTreeMap` keyed by `(peer, SDU)`, so the
//!   order in which timed-out sets are retired — and therefore the order of the records they
//!   emit — is a function of ids and not of insertion history.
//! * **Ordered reductions.** The amplification product runs over fragments sorted by index;
//!   the content-weighted mean uses [`v2xw_core::math::sum_sorted_by_key`]; per-node meters
//!   merge through [`amplification::AmplificationMeter::merged`], which sorts by
//!   [`v2xw_core::ids::NodeId`] first (02-architecture.md §6.4).
//! * **Quantisation at the writer.** The only floats this crate exports are the
//!   amplification figures, and they go out through
//!   [`amplification::AmplificationMeter::snapshot`], which puts every one on the
//!   three-decimal grid (build decision D9).
//! * **Integer byte counts.** [`accounting::ByteLedger`] counts in `u64`, so merging per-node
//!   ledgers needs no ordering at all.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod accounting;
pub mod amplification;
pub mod error;
pub mod frag;
pub mod frame;
pub mod gn;
pub mod netlayer;
pub mod wsmp;

#[cfg(test)]
mod testctx;

pub use accounting::{Bucket, ByteLedger, BytesOnWire};
pub use amplification::{
    AmplificationMeter, AmplificationSnapshot, FragmentLoss, SduLossModel, equal_p_loss, sdu_loss,
};
pub use error::{DropCause, NetError, Result};
pub use frag::cert_cycle::{CertCycleParams, CertCyclePartialHybrid, FRAGMENTER_CERT_CYCLE_ID};
pub use frag::facilities::{FRAGMENTER_FACILITIES_ID, FacilitiesParams, FacilitiesSegmentation};
pub use frag::generic::{FRAGMENTER_GENERIC_ID, GenericParams, GenericSduFragmenter};
pub use frag::none::{FRAGMENTER_NONE_ID, NoneFragmenter};
pub use frag::{
    FragRecord, FragmentDesc, FragmentKind, Fragmenter, ReassemblyBuffer, ReassemblyOutcome,
};
pub use frame::{FCS_BYTES, FrameLayers, FrameMsg, MAC_HEADER_QOS_DATA_BYTES, NetStack};
pub use gn::{GN_BTP_NET_LAYER_ID, GnBtpNetLayer, GnParams, ITS_GN_MAX_SDU_SIZE};
pub use netlayer::{
    BtpKind, BtpPort, DecapOutcome, GnTransport, NetLayer, NetMeta, NetPdu, Psid, Transport,
    WsmpExtensions,
};
pub use wsmp::{WSMP_NET_LAYER_ID, WsmpNetLayer, WsmpParams};

/// Every model this crate registers, as `(id, family)` pairs, in a fixed order.
///
/// The list a scenario validator or a documentation generator walks; fixed order rather than
/// a map, so two runs print it the same way.
pub const MODELS: [(&str, v2xw_core::card::Family); 6] = [
    (WSMP_NET_LAYER_ID, v2xw_core::card::Family::Net),
    (GN_BTP_NET_LAYER_ID, v2xw_core::card::Family::Net),
    (FRAGMENTER_NONE_ID, v2xw_core::card::Family::Fragmenter),
    (
        FRAGMENTER_FACILITIES_ID,
        v2xw_core::card::Family::Fragmenter,
    ),
    (
        FRAGMENTER_CERT_CYCLE_ID,
        v2xw_core::card::Family::Fragmenter,
    ),
    (FRAGMENTER_GENERIC_ID, v2xw_core::card::Family::Fragmenter),
];
