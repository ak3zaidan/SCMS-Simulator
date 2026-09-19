//! `net/gn-btp/en302636` — the ETSI GeoNetworking and BTP header sizes of 04-models.md §7.2.
//!
//! Every size here is VERIFIED against the standards' own tables: EN 302 636-4-1 V1.4.1
//! §9.5-9.7 and Tables 11-17 for GeoNetworking, EN 302 636-5-1 V2.2.1 §7.2-7.3 for BTP
//! (R4 §F.2-F.3).
//!
//! | Header | Octets |
//! |---|---|
//! | Basic Header | 4 |
//! | Common Header | 8 |
//! | Long Position Vector | 24 |
//! | Short Position Vector | 20 |
//! | SHB (CAM, CPM, VAM) | 40 |
//! | TSB | 40 |
//! | GBC / GAC (DENM) | 56 |
//! | GUC | 60 |
//! | BEACON | 36 |
//! | BTP-A / BTP-B | 4 |
//! | LLC/SNAP (ITS-G5) | 8 (DERIVED) |
//! | **Below a secured CAM** | **52** (LLC/SNAP 8 + SHB 40 + BTP-B 4, DERIVED) |
//!
//! The per-transport totals are composed from the field sizes in
//! [`crate::netlayer::GnTransport::header_bytes`], so the code is the composition column
//! and the tests pin the documented totals.
//!
//! # The MTU rule, and an arithmetic discrepancy in the sources
//!
//! EN 302 636-4-1 §9.2.3 gives `MTU_GN ≤ MTU_AL − GEO_MAX`, and Annex H items 8-9 give two
//! VERIFIED numbers: `itsGnMaxSduSize` is **1,398** octets and
//! `itsGnMaxGeoNetworkingHeaderSize` (GN_MAX) is **88** octets — the GeoUnicast header
//! *including security accounting*.
//!
//! 04-models.md §7.2 and R4 §F.2 both restate the first as the identity
//! "1,398 = 1,500 − 88 − 0". **That arithmetic does not hold**: 1,500 − 88 is 1,412, and
//! 1,398 + 88 is 1,486, so whatever `MTU_AL` the MIB's own subtraction used was not 1,500.
//! Both the 1,398 and the 88 are VERIFIED against Annex H, and only the quoted `MTU_AL` and
//! the subtraction connecting them are not, so this model keeps the two verified numbers and
//! does **not** derive one from the other:
//!
//! * [`GnBtpNetLayer::sdu_mtu`] is `min(itsGnMaxSduSize, access-layer limit)` — the MIB's
//!   provisioned maximum, bounded by what the access layer can actually carry;
//! * [`GnBtpNetLayer::access_layer_limit`] is §9.2.3's rule,
//!   `MTU_AL − GN_MAX − GNSEC_MAX`, which is what binds when a scenario configures a
//!   smaller access layer or a non-zero security allowance.
//!
//! With the defaults the MIB value binds and the MTU is 1,398, which is the number that
//! matters; the discrepancy is recorded as a limitation on the model card rather than
//! resolved by picking an arithmetic that no source states.
//!
//! The 88 octets are 24 more than the largest header this module sizes (GUC 60 + BTP 4),
//! and that difference is the security allowance the MIB folds into GN_MAX. It is not an
//! inconsistency: GN_MAX is a *provisioned ceiling* for the MTU calculation, while
//! [`crate::netlayer::NetLayer::header_bytes`] is the exact size of the header a given
//! packet carries.
//!
//! # No fragmentation, at this layer
//!
//! The full text of EN 302 636-4-1 V1.4.1 contains zero occurrences of "fragment"
//! [R4 §F.2, VERIFIED by absence]. An SDU above the MTU is refused here
//! ([`crate::error::NetError::Oversize`]); oversize content is handled **at the facilities
//! layer** — MAPEM's `layerID` fragmentation [TS 103 301 §6.4.1] and CPM's
//! `messageSegmentInfo` segmentation [TS 103 324 §6.1.2.1] — which is
//! [`crate::frag::facilities`], not this module.
//!
//! GeoNetworking's multi-hop forwarding (GBC forwarding, DENM keep-alive forwarding) and
//! duplicate-packet detection belong to the `high` net tier (04-models.md §7.5) and are not
//! implemented; [`crate::netlayer::DecapOutcome`] documents the consequence.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::registry::ParamSet;

use crate::error::{DropCause, MalformedPdu, NetError, Result};
use crate::netlayer::{DecapOutcome, LLC_SNAP_BYTES, NetLayer, NetMeta, NetPdu, Transport};

/// This model's stable id.
pub const GN_BTP_NET_LAYER_ID: &str = "net/gn-btp/en302636";

/// The access-layer MTU the `itsGnMaxSduSize` calculation starts from, octets
/// [EN 302 636-4-1 V1.4.1 Annex H item 8, VERIFIED].
pub const MTU_AL_BYTES: u32 = 1_500;

/// `itsGnMaxGeoNetworkingHeaderSize` (GN_MAX), octets: the GeoUnicast header including
/// security accounting [EN 302 636-4-1 V1.4.1 Annex H item 9, VERIFIED].
pub const ITS_GN_MAX_GEONETWORKING_HEADER_SIZE: u32 = 88;

/// `itsGnMaxSduSize`, octets: the MIB's provisioned maximum SDU
/// [EN 302 636-4-1 V1.4.1 Annex H item 8, VERIFIED].
///
/// The quoted derivation `1,500 − 88 − 0` does not evaluate to this number; see the module
/// documentation. The value itself is the standard's.
pub const ITS_GN_MAX_SDU_SIZE: u32 = 1_398;

/// Everything below a secured CAM on ITS-G5: LLC/SNAP 8 + GN SHB 40 + BTP-B 4 = 52 octets
/// (04-models.md §7.2, R4 §F.3, DERIVED).
pub const BELOW_SECURED_CAM_BYTES: u32 = 52;

/// The tunable parameters of the GeoNetworking/BTP layer: the three numbers the MTU rule of
/// §9.2.3 and Annex H is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnParams {
    /// `itsGnMaxSduSize`: the MIB's provisioned maximum SDU, octets. Default
    /// [`ITS_GN_MAX_SDU_SIZE`].
    pub its_gn_max_sdu_size: u32,
    /// `MTU_AL`: the access layer's MTU, octets. Default [`MTU_AL_BYTES`].
    pub mtu_al_bytes: u32,
    /// `GN_MAX` = `itsGnMaxGeoNetworkingHeaderSize`, octets. Default
    /// [`ITS_GN_MAX_GEONETWORKING_HEADER_SIZE`].
    pub max_gn_header_bytes: u32,
    /// `GNSEC_MAX`: the security-header allowance Annex H item 8 sets to 0, octets.
    pub max_gn_security_bytes: u32,
}

impl Default for GnParams {
    fn default() -> Self {
        Self {
            its_gn_max_sdu_size: ITS_GN_MAX_SDU_SIZE,
            mtu_al_bytes: MTU_AL_BYTES,
            max_gn_header_bytes: ITS_GN_MAX_GEONETWORKING_HEADER_SIZE,
            max_gn_security_bytes: 0,
        }
    }
}

/// `net/gn-btp/en302636`: GeoNetworking and BTP header sizes, the `itsGnMaxSduSize` MTU
/// rule, and no fragmentation.
///
/// ```
/// use v2xw_net::gn::{BELOW_SECURED_CAM_BYTES, GnBtpNetLayer, ITS_GN_MAX_SDU_SIZE};
/// use v2xw_net::netlayer::NetMeta;
///
/// let layer = GnBtpNetLayer::default();
/// // A 357 B secured CAM — the C2C-CC TR 2052 field mean (04-models.md §8.2).
/// assert_eq!(layer.header_size(&NetMeta::for_cam(357)), 52);
/// assert_eq!(BELOW_SECURED_CAM_BYTES, 52);
/// assert_eq!(layer.sdu_mtu(), ITS_GN_MAX_SDU_SIZE);
/// ```
#[derive(Debug, Clone)]
pub struct GnBtpNetLayer {
    params: GnParams,
    card: ModelCard,
}

impl Default for GnBtpNetLayer {
    fn default() -> Self {
        Self::new(GnParams::default())
    }
}

impl GnBtpNetLayer {
    /// A layer with the given parameters.
    pub fn new(params: GnParams) -> Self {
        Self {
            params,
            card: card(),
        }
    }

    /// A layer configured from a resolved parameter set (invariant I-C3: every name read
    /// here is declared on the card).
    pub fn from_params(params: &ParamSet) -> Self {
        let d = GnParams::default();
        let read = |name: &str, fallback: u32| {
            params
                .get_u64(name)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback)
        };
        Self::new(GnParams {
            its_gn_max_sdu_size: read("its_gn_max_sdu_size", d.its_gn_max_sdu_size),
            mtu_al_bytes: read("mtu_al_bytes", d.mtu_al_bytes),
            max_gn_header_bytes: read("max_gn_header_bytes", d.max_gn_header_bytes),
            max_gn_security_bytes: read("max_gn_security_bytes", d.max_gn_security_bytes),
        })
    }

    /// The parameters in force.
    pub const fn params(&self) -> &GnParams {
        &self.params
    }

    /// §9.2.3's rule, `MTU_AL − GN_MAX − GNSEC_MAX`: the largest SDU the access layer can
    /// carry once the provisioned header ceiling is reserved.
    ///
    /// Saturating: a scenario that provisioned a header ceiling above the access layer's
    /// MTU has a limit of zero, which refuses every SDU, rather than a wrapped enormous one
    /// that would accept all of them.
    pub const fn access_layer_limit(&self) -> u32 {
        self.params
            .mtu_al_bytes
            .saturating_sub(self.params.max_gn_header_bytes)
            .saturating_sub(self.params.max_gn_security_bytes)
    }

    /// The largest SDU this layer accepts: the smaller of the MIB's `itsGnMaxSduSize` and
    /// [`GnBtpNetLayer::access_layer_limit`].
    ///
    /// With the standard's own defaults the MIB value binds and this is 1,398 octets. See
    /// the module documentation for why the two are not derived from one another.
    pub const fn sdu_mtu(&self) -> u32 {
        let limit = self.access_layer_limit();
        if self.params.its_gn_max_sdu_size < limit {
            self.params.its_gn_max_sdu_size
        } else {
            limit
        }
    }

    /// The header size for `meta`, as a plain function — the body of
    /// [`NetLayer::header_bytes`], callable without naming a context type.
    ///
    /// `GN transport header + BTP header [+ LLC/SNAP]`.
    pub const fn header_size(&self, meta: &NetMeta) -> u32 {
        let llc = if meta.include_llc_snap {
            LLC_SNAP_BYTES
        } else {
            0
        };
        meta.gn.header_bytes() + meta.btp.header_bytes() + llc
    }
}

impl Model for GnBtpNetLayer {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> NetLayer<C> for GnBtpNetLayer
where
    C: Ctx + ?Sized,
{
    fn header_bytes(&self, meta: &NetMeta) -> u32 {
        self.header_size(meta)
    }

    fn encapsulate(&self, sdu: &[u8], meta: &NetMeta) -> Result<Vec<NetPdu>> {
        let sdu_bytes = u32::try_from(sdu.len()).unwrap_or(u32::MAX);
        let mtu = self.sdu_mtu();
        if sdu_bytes > mtu {
            return Err(NetError::Oversize {
                layer: GN_BTP_NET_LAYER_ID,
                sdu_bytes,
                mtu,
            });
        }
        let meta = meta.with_sdu_bytes(sdu_bytes);
        Ok(vec![NetPdu {
            layer: GN_BTP_NET_LAYER_ID,
            header_bytes: self.header_size(&meta),
            transport: Transport::Btp {
                kind: meta.btp,
                port: meta.port,
            },
            gn: Some(meta.gn),
            payload: sdu.to_vec(),
        }])
    }

    fn decapsulate(&mut self, _ctx: &mut C, _rx: NodeId, pdu: &NetPdu) -> DecapOutcome {
        match pdu.transport {
            Transport::Btp { kind, port } => {
                if pdu.gn.is_none() {
                    // A BTP PDU with no GeoNetworking transport type was not produced by
                    // this layer: the header size it claims cannot be checked against a
                    // shape.
                    return DecapOutcome::Drop(DropCause::MalformedPdu {
                        detail: MalformedPdu::MissingGnTransport,
                    });
                }
                DecapOutcome::Deliver {
                    sdu: pdu.payload.clone(),
                    transport: Transport::Btp { kind, port },
                }
            }
            Transport::Wsmp { .. } => DecapOutcome::Drop(DropCause::MalformedPdu {
                detail: MalformedPdu::ForeignLayer,
            }),
        }
    }

    fn mtu(&self) -> u32 {
        self.sdu_mtu()
    }
}

/// The model card for `net/gn-btp/en302636`.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        GN_BTP_NET_LAYER_ID,
        Family::Net,
        "1.0.0",
        "Exact ETSI GeoNetworking and BTP header sizes (Basic 4, Common 8, LPV 24, SPV 20, \
         SHB 40, TSB 40, GBC/GAC 56, GUC 60, BEACON 36, BTP 4, LLC/SNAP 8) and the \
         itsGnMaxSduSize MTU rule. Fragments nothing: oversize content is segmented at the \
         facilities layer.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "header size".to_string(),
            latex_or_text: "H = GN(transport) + BTP + [LLC/SNAP]".to_string(),
            notes: Some(
                "GN(SHB) = 4 + 8 + 24 + 4 = 40; GN(GBC) = 4 + 8 + 2 + 2 + 24 + 4 + 4 + 2 + 2 \
                 + 2 + 2 = 56; GN(GUC) = 4 + 8 + 2 + 2 + 24 + 20 = 60; BTP-A and BTP-B are 4 \
                 each. Below a secured CAM on ITS-G5: 8 + 40 + 4 = 52."
                    .to_string(),
            ),
        },
        Equation {
            name: "MTU rule".to_string(),
            latex_or_text: "MTU = min(itsGnMaxSduSize, MTU_AL - GN_MAX - GNSEC_MAX)                             = min(1398, 1500 - 88 - 0) = 1398"
                .to_string(),
            notes: Some(
                "EN 302 636-4-1 §9.2.3 states MTU_GN <= MTU_AL - GEO_MAX; Annex H items 8-9                  give itsGnMaxSduSize = 1 398 and GN_MAX = 88, both VERIFIED. The identity                  '1 398 = 1 500 - 88 - 0' quoted by 04-models.md §7.2 does not evaluate                  (1 500 - 88 = 1 412), so the two verified numbers are kept separate and the                  smaller binds. GN_MAX is a provisioned ceiling that already includes a                  security allowance, which is why it exceeds the largest header this model                  sizes (GUC 60 + BTP 4)."
                    .to_string(),
            ),
        },
    ];
    let annex_h = |item: &str| Source {
        kind: SourceKind::Standard,
        reference: format!("ETSI EN 302 636-4-1 V1.4.1 Annex H {item}"),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    card.parameters = vec![
        Parameter {
            name: "its_gn_max_sdu_size".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(ITS_GN_MAX_SDU_SIZE),
            range: Some(vec![serde_json::json!(64), serde_json::json!(9_000)]),
            source: annex_h("item 8 (itsGnMaxSduSize = 1 398 octets)"),
            calibration: None,
        },
        Parameter {
            name: "mtu_al_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(MTU_AL_BYTES),
            range: Some(vec![serde_json::json!(64), serde_json::json!(9_000)]),
            source: annex_h(
                "item 8 quotes MTU_AL = 1 500 in the itsGnMaxSduSize calculation; §9.2.3 \
                 gives the rule MTU_GN <= MTU_AL - GEO_MAX",
            ),
            calibration: None,
        },
        Parameter {
            name: "max_gn_header_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(ITS_GN_MAX_GEONETWORKING_HEADER_SIZE),
            range: Some(vec![serde_json::json!(60), serde_json::json!(255)]),
            source: annex_h("item 9 (itsGnMaxGeoNetworkingHeaderSize = 88)"),
            calibration: None,
        },
        Parameter {
            name: "max_gn_security_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(0),
            range: Some(vec![serde_json::json!(0), serde_json::json!(255)]),
            source: annex_h("item 8 (GNSEC_MAX = 0 in the quoted calculation)"),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "ITS-G5 framing: the eight LLC/SNAP octets are counted at this layer when \
         NetMeta::include_llc_snap is set, which is the default for the GeoNetworking \
         constructors, because the documented 52-octet figure below a secured CAM includes \
         them. Invariant I-N1 requires that the MAC model then does not count them again."
            .to_string(),
        "One position vector per packet, of the size its transport type's table specifies; \
         the optional GN extension headers (secured-packet, IPv6 adaptation) are out of \
         scope at these tiers."
            .to_string(),
        "Header sizes are exact; the header's octets are not synthesised (see the crate \
         documentation)."
            .to_string(),
    ];
    card.limitations = vec![
        "The LLC/SNAP figure of 8 octets is DERIVED from IEEE 802.2 plus SNAP (DSAP/SSAP \
         0xAA, control 0x03, OUI 00-00-00, EtherType). EN 302 663 §4.3.1 Figure 3 shows \
         \"IEEE/ISO/IEC 8802-2 with SNAP\" but does not quote the octet count, and \
         TS 102 636-4-2 V1.1.1 does not contain it (R4 §F.2)."
            .to_string(),
        "The 52-octet total below a secured CAM is DERIVED by addition from three VERIFIED \
         sizes; no capture was measured against it."
            .to_string(),
        "The identity '1 398 = 1 500 - 88 - 0' that 04-models.md §7.2 and R4 §F.2 quote for \
         itsGnMaxSduSize does not evaluate: 1 500 - 88 is 1 412, and 1 398 + 88 is 1 486, so \
         the MTU_AL the MIB's own subtraction used was not 1 500. The two Annex H values \
         (1 398 and 88) are VERIFIED and are both kept; the MTU is the smaller of the MIB \
         value and §9.2.3's rule, and no source is stretched to make the quoted subtraction \
         come out."
            .to_string(),
        "GeoNetworking does not fragment: the full text of EN 302 636-4-1 V1.4.1 contains \
         zero occurrences of \"fragment\" (VERIFIED by absence). An oversize SDU is refused \
         here, and oversize content is segmented at the facilities layer (MAPEM layerID, \
         CPM messageSegmentInfo) by the fragmenter/facilities-segmentation model."
            .to_string(),
        "The CAM (2001), DENM (2002) and SSEM (2008) BTP ports are attributed to \
         TS 103 248, which was not extracted, and are UNVERIFIED. A port changes no header \
         size — a BTP header is four octets whatever the port — so the exposure is \
         demultiplexing inside the model only."
            .to_string(),
    ];
    card.ignores = vec![
        "GN forwarding: GBC multi-hop forwarding, the DENM keep-alive forwarding algorithm, \
         the location table, duplicate-packet detection and the contention-based forwarding \
         timers, all of which belong to the high net tier (04-models.md §7.2, §7.5)."
            .to_string(),
        "DCC_NET, the GeoNetworking CBR-sharing mechanism of TS 102 636-4-2 \
         (04-models.md §6)."
            .to_string(),
        "The secured-packet extension header: the security envelope's own bytes are \
         v2xw-sec's (04-models.md §9.1) and arrive here inside the SDU."
            .to_string(),
    ];
    card.sources = vec![
        Source {
            kind: SourceKind::Standard,
            reference: "ETSI EN 302 636-4-1 V1.4.1 (2020-01) §9.5-9.7, §9.2.3, Tables 11-17, \
                        Annex H"
                .to_string(),
            accessed: Some("2026-09-17".to_string()),
            note: Some("Header sizes and the MTU rule, all VERIFIED.".to_string()),
        },
        Source {
            kind: SourceKind::Standard,
            reference: "ETSI EN 302 636-5-1 V2.2.1 (2019-05) §7.2-7.3, Tables 1-3".to_string(),
            accessed: Some("2026-09-17".to_string()),
            note: Some("BTP-A and BTP-B, 4 octets each, VERIFIED.".to_string()),
        },
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 301 CSP_PortNo tables — well-known BTP ports (MAPEM 2003, \
             SPATEM 2004, IVIM 2006, SREM 2007, RTCMEM 2013)",
        ),
        Source::new(
            SourceKind::Paper,
            "docs/design/research/R4-messages-envelopes.md §F.2-F.3 — the fact sheet these \
             sizes were extracted from",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §7.2 — every documented size, and the 52-octet total below a \
             secured CAM",
        )],
        tests: vec![
            "gn::tests::the_total_below_a_secured_cam_is_fifty_two_bytes".to_string(),
            "gn::tests::every_documented_header_size_matches".to_string(),
            "gn::tests::the_mtu_is_the_annex_h_value_bounded_by_the_access_layer".to_string(),
            "gn::tests::an_oversize_sdu_is_refused_because_gn_does_not_fragment".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlayer::{BTP_HEADER_BYTES, BtpKind, BtpPort, GnTransport, Psid};
    use crate::testctx::TestCtx;

    fn header(l: &GnBtpNetLayer, meta: &NetMeta) -> u32 {
        <GnBtpNetLayer as NetLayer<TestCtx>>::header_bytes(l, meta)
    }

    /// The headline figure of 04-models.md §7.2: "Below a secured CAM | 52 | LLC/SNAP 8 +
    /// SHB 40 + BTP-B 4 (DERIVED)".
    #[test]
    fn the_total_below_a_secured_cam_is_fifty_two_bytes() {
        let l = GnBtpNetLayer::default();
        // 357 B is the overall field mean of a secured CAM (C2C-CC TR 2052, §8.2).
        assert_eq!(header(&l, &NetMeta::for_cam(357)), 52);
        assert_eq!(BELOW_SECURED_CAM_BYTES, 52);
        // …and it is that sum, term by term.
        assert_eq!(
            LLC_SNAP_BYTES + GnTransport::Shb.header_bytes() + BtpKind::B.header_bytes(),
            52
        );
        // The size does not depend on the CAM's own size: headers are fixed-width here.
        for cam_bytes in [182u32, 199, 297, 357, 406, 807] {
            assert_eq!(header(&l, &NetMeta::for_cam(cam_bytes)), 52);
        }
    }

    /// Every size in the §7.2 table, through the layer.
    #[test]
    fn every_documented_header_size_matches() {
        let l = GnBtpNetLayer::default();
        // The GN transport headers on their own (no BTP, no LLC/SNAP).
        let bare = |gn: GnTransport| {
            header(
                &l,
                &NetMeta::gn(100, gn, BtpKind::B, BtpPort::CAM).with_llc_snap(false),
            ) - BtpKind::B.header_bytes()
        };
        assert_eq!(bare(GnTransport::Beacon), 36);
        assert_eq!(bare(GnTransport::Shb), 40);
        assert_eq!(bare(GnTransport::Tsb), 40);
        assert_eq!(bare(GnTransport::Gbc), 56);
        assert_eq!(bare(GnTransport::Gac), 56);
        assert_eq!(bare(GnTransport::Guc), 60);

        // A DENM over GBC with BTP-B and LLC/SNAP: 8 + 56 + 4.
        assert_eq!(header(&l, &NetMeta::for_denm(300)), 68);
        // BTP-A costs the same as BTP-B.
        let a = NetMeta::gn(300, GnTransport::Gbc, BtpKind::A, BtpPort::SREM);
        assert_eq!(header(&l, &a), 68);
        assert_eq!(BTP_HEADER_BYTES, 4);
    }

    /// The MTU is Annex H's own `itsGnMaxSduSize`, bounded by §9.2.3's access-layer rule.
    #[test]
    fn the_mtu_is_the_annex_h_value_bounded_by_the_access_layer() {
        let l = GnBtpNetLayer::default();
        assert_eq!(l.sdu_mtu(), 1_398);
        assert_eq!(ITS_GN_MAX_SDU_SIZE, 1_398);
        assert_eq!(<GnBtpNetLayer as NetLayer<TestCtx>>::mtu(&l), 1_398);

        // The identity the sources quote does not evaluate, which is why the two VERIFIED
        // numbers are kept apart rather than one being derived from the other.
        assert_eq!(l.access_layer_limit(), 1_412);
        assert_ne!(
            MTU_AL_BYTES - ITS_GN_MAX_GEONETWORKING_HEADER_SIZE,
            ITS_GN_MAX_SDU_SIZE,
            "'1398 = 1500 - 88 - 0' is not arithmetic that holds; see the card's limitation"
        );
        assert_eq!(
            ITS_GN_MAX_SDU_SIZE + ITS_GN_MAX_GEONETWORKING_HEADER_SIZE,
            1_486,
            "the MTU_AL the MIB's own subtraction used"
        );

        // GN_MAX is a provisioned ceiling: it exceeds the largest header actually sized,
        // by the security allowance the MIB folds into it.
        let largest = GnTransport::Guc.header_bytes() + BtpKind::B.header_bytes();
        assert_eq!(largest, 64);
        assert!(largest < ITS_GN_MAX_GEONETWORKING_HEADER_SIZE);

        // A security allowance big enough to bite takes over from the MIB value.
        let secured = GnBtpNetLayer::new(GnParams {
            max_gn_security_bytes: 141,
            ..GnParams::default()
        });
        assert_eq!(secured.access_layer_limit(), 1_500 - 88 - 141);
        assert_eq!(secured.sdu_mtu(), 1_271);

        // And an over-provisioned ceiling saturates at zero rather than wrapping.
        let absurd = GnBtpNetLayer::new(GnParams {
            mtu_al_bytes: 64,
            ..GnParams::default()
        });
        assert_eq!(absurd.sdu_mtu(), 0);
    }

    #[test]
    fn an_oversize_sdu_is_refused_because_gn_does_not_fragment() {
        let l = GnBtpNetLayer::default();
        assert!(!<GnBtpNetLayer as NetLayer<TestCtx>>::fragments(&l));

        let meta = NetMeta::for_cam(1_399);
        let sdu = vec![0u8; 1_399];
        assert_eq!(
            <GnBtpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &sdu, &meta),
            Err(NetError::Oversize {
                layer: GN_BTP_NET_LAYER_ID,
                sdu_bytes: 1_399,
                mtu: 1_398,
            })
        );

        // Exactly at the MTU it is accepted, as a single PDU.
        let sdu = vec![0u8; 1_398];
        let pdus = <GnBtpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &sdu, &meta).unwrap();
        assert_eq!(pdus.len(), 1);
        assert_eq!(pdus[0].header_bytes, 52);
        assert_eq!(pdus[0].total_bytes(), 1_398 + 52);
        assert_eq!(pdus[0].gn, Some(GnTransport::Shb));
    }

    #[test]
    fn decapsulation_delivers_the_sdu_and_rejects_a_foreign_pdu() {
        let mut l = GnBtpNetLayer::default();
        let mut ctx = TestCtx::new();
        let pdus = <GnBtpNetLayer as NetLayer<TestCtx>>::encapsulate(
            &l,
            &[5u8; 40],
            &NetMeta::for_cam(40),
        )
        .unwrap();
        assert_eq!(
            l.decapsulate(&mut ctx, NodeId::new(1), &pdus[0]),
            DecapOutcome::Deliver {
                sdu: vec![5u8; 40],
                transport: Transport::Btp {
                    kind: BtpKind::B,
                    port: BtpPort::CAM
                },
            }
        );

        let foreign = NetPdu {
            layer: "net/wsmp/1609-3",
            header_bytes: 5,
            transport: Transport::Wsmp { psid: Psid::BSM },
            gn: None,
            payload: vec![1, 2],
        };
        assert!(matches!(
            l.decapsulate(&mut ctx, NodeId::new(1), &foreign),
            DecapOutcome::Drop(DropCause::MalformedPdu { .. })
        ));

        let headerless = NetPdu {
            gn: None,
            ..pdus[0].clone()
        };
        assert!(matches!(
            l.decapsulate(&mut ctx, NodeId::new(1), &headerless),
            DecapOutcome::Drop(DropCause::MalformedPdu { .. })
        ));
    }

    #[test]
    fn the_card_validates_and_declares_no_rng() {
        let l = GnBtpNetLayer::default();
        let card = l.card();
        card.validate().expect("card validates");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Net);
        assert!(!card.determinism.uses_rng);
        assert_eq!(card.parameters.len(), 4);
    }

    #[test]
    fn parameters_come_from_the_resolved_set() {
        let card = card();
        let params = ParamSet::resolve(
            &card,
            &serde_json::json!({"mtu_al_bytes": 1_492, "max_gn_security_bytes": 141}),
        )
        .expect("both overrides are declared and in range");
        let l = GnBtpNetLayer::from_params(&params);
        assert_eq!(l.params().mtu_al_bytes, 1_492);
        assert_eq!(l.access_layer_limit(), 1_492 - 88 - 141);
        assert_eq!(
            l.sdu_mtu(),
            1_263,
            "the access layer now binds, not the MIB"
        );
    }
}
