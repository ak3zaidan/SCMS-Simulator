//! `net/wsmp/1609-3` — the IEEE 1609.3 WSMP header layout of 04-models.md §7.1.
//!
//! WSMP v3 (IEEE 1609.3-2016/2020) puts between four and fourteen octets in front of the
//! SDU, and the exact number is a function of four things: how many of the three optional
//! N-header extensions are present, how wide the p-encoded PSID is, how wide the length
//! field is, and nothing else. This module is that function, plus the MTU and the fact that
//! **WSMP does not fragment**.
//!
//! | Field | Octets | Notes |
//! |---|---|---|
//! | WSMP-N-Header first octet | 1 | subtype 4 bits, option indicator 1 bit, version 3 bits |
//! | N-header extensions | 3 each | channel number (id 15), data rate (id 16), transmit power (id 4): element id 1 + length 1 + one-octet value |
//! | TPID | 1 | selects the PSID-only versus the port-carrying transport PDU |
//! | WSMP-T PSID | 1-4 | p-encoded `VarLengthNumber`, values up to `0x1020407F` |
//! | WSMP-T Length | 1-2 | variable |
//!
//! [Wireshark `packet-wsmp.c` (`dissect_wsmp_v3`); IEEE 1609.3 ASN.1 `wsm.asn`, `wee.asn`;
//! IEEE PSID tutorial; R4 §F.1. All VERIFIED as secondary sources or from the ASN.1 — the
//! standard itself is paywalled.]
//!
//! # The two derivations
//!
//! **The default BSM header is 5 octets.** N-header 1 + TPID 1 + PSID 1 (`0x20` is one
//! p-encoded octet) + length 2. 04-models.md §7.1 states that total, and
//! [`WSMP_BSM_HEADER_BYTES`] is it; with all three extensions the same header is 14.
//!
//! **The length field is two octets once the payload reaches 128 bytes.** R4 §F.1 gives the
//! minimum WSMP overhead as 4 octets with a one-octet length, and 04-models.md §7.1 gives
//! the BSM total as 5 with a two-octet length "payload ≥ 128 B". The only rule that
//! produces both is the p-encoding boundary at `0x80`, which is the same rule the PSID
//! follows, so this module uses [`crate::netlayer::p_encoded_bytes`] for the length field
//! too. The standard's clause for the length field's encoding is not accessible, so the
//! model card records this as DERIVED with the two documented totals as its anchors.
//!
//! # What this model does not do
//!
//! * **No fragmentation.** `ShortMsgNpdu` and `ShortMsgBcPDU` have no fragmentation field,
//!   `ShortMsgData ::= OCTET STRING` is annotated "maximum size is given by access
//!   technology", and Wireshark's dissector has no reassembly [R4 §F.1, VERIFIED by
//!   absence]. An oversize SDU is refused with [`crate::error::NetError::Oversize`]; a
//!   [`crate::frag::Fragmenter`] has to act above this layer.
//! * **No UDP/IPv6 or TCP/IPv6.** 1609.3 offers all three; 04-models.md §7.1 puts the IP
//!   alternatives out of scope for Phases 1-2, so only the PSID-addressed WSMP forms exist
//!   here.
//! * **No 1609.4 channel switching.** The channel-number extension is sized, not acted on.

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
use crate::netlayer::{
    DecapOutcome, LLC_SNAP_BYTES, NetLayer, NetMeta, NetPdu, Transport, p_encoded_bytes,
};

/// This model's stable id.
pub const WSMP_NET_LAYER_ID: &str = "net/wsmp/1609-3";

/// The WSMP-N-Header's first octet: subtype 4 bits, option indicator 1 bit, version 3 bits
/// (04-models.md §7.1).
pub const WSMP_N_HEADER_BYTES: u32 = 1;

/// The TPID octet, which selects the PSID-only versus the port-carrying transport PDU
/// (04-models.md §7.1).
pub const WSMP_TPID_BYTES: u32 = 1;

/// The smallest possible WSMP header: N-header 1 + TPID 1 + one-octet PSID + one-octet
/// length (04-models.md §7.1, R4 §F.1, DERIVED).
pub const WSMP_MIN_HEADER_BYTES: u32 = 4;

/// The default WSMP header for a BSM: 5 octets (04-models.md §7.1, DERIVED).
///
/// N-header 1 + TPID 1 + PSID 1 (`0x20`) + length 2 (payload ≥ 128 B).
pub const WSMP_BSM_HEADER_BYTES: u32 = 5;

/// The same BSM header with all three N-header extensions: 14 octets (04-models.md §7.1,
/// DERIVED).
pub const WSMP_BSM_HEADER_BYTES_WITH_ALL_EXTENSIONS: u32 = 14;

/// The IEEE 1609.3-2020 MIB default for the maximum WSM payload, bytes
/// [CTI 4501 §4.3.3.1.3.1 via 04-models.md §7.1, VERIFIED secondary].
pub const WSM_MAX_PAYLOAD_MIB_DEFAULT: u32 = 1_400;

/// The largest WSM payload 1609.3 supports, bytes [CTI 4501 §4.3.3.1.3.1, VERIFIED
/// secondary].
pub const WSM_MAX_PAYLOAD_SUPPORTED: u32 = 2_302;

/// The 802.11 MSDU cap, bytes — the hard ceiling below which every WSM must fit
/// [04-models.md §4.6, §7.1; 802.11 Table 9-25 via NDSS 2024, VERIFIED secondary].
pub const MAX_MSDU_BYTES: u32 = 2_304;

/// The largest payload a two-octet p-encoded length field can express, bytes.
///
/// Far above [`MAX_MSDU_BYTES`], which is why the length field never needs a third octet in
/// practice; [`WsmpNetLayer::encapsulate`] refuses a payload that would need one.
pub const WSMP_MAX_LENGTH_FIELD_PAYLOAD: u32 = 0x407F;

/// The tunable parameters of the WSMP layer. One number, and it is the MIB's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsmpParams {
    /// The maximum WSM payload, bytes. Default [`WSM_MAX_PAYLOAD_MIB_DEFAULT`]; the
    /// standard supports up to [`WSM_MAX_PAYLOAD_SUPPORTED`].
    pub max_wsm_payload_bytes: u32,
}

impl Default for WsmpParams {
    fn default() -> Self {
        Self {
            max_wsm_payload_bytes: WSM_MAX_PAYLOAD_MIB_DEFAULT,
        }
    }
}

/// `net/wsmp/1609-3`: the WSMP header sizes, the MTU, and no fragmentation.
///
/// [`NetLayer::header_bytes`] needs a context type to name the trait, so the size is also
/// reachable as the plain function [`WsmpNetLayer::header_size`], which is what its trait
/// method calls:
///
/// ```
/// use v2xw_net::netlayer::{NetMeta, WsmpExtensions};
/// use v2xw_net::wsmp::{WSMP_BSM_HEADER_BYTES, WsmpNetLayer};
///
/// let layer = WsmpNetLayer::default();
/// // A 180 B BSM SPDU (Rostami 2018's digest-signed size, 04-models.md §9.3).
/// let meta = NetMeta::for_bsm(180);
/// assert_eq!(layer.header_size(&meta), 5);
/// assert_eq!(WSMP_BSM_HEADER_BYTES, 5);
///
/// // …and 14 with the channel, data-rate and transmit-power extensions.
/// assert_eq!(layer.header_size(&meta.with_extensions(WsmpExtensions::ALL)), 14);
/// ```
#[derive(Debug, Clone)]
pub struct WsmpNetLayer {
    params: WsmpParams,
    card: ModelCard,
}

impl Default for WsmpNetLayer {
    fn default() -> Self {
        Self::new(WsmpParams::default())
    }
}

impl WsmpNetLayer {
    /// A layer with the given parameters.
    pub fn new(params: WsmpParams) -> Self {
        Self {
            params,
            card: card(),
        }
    }

    /// A layer configured from a resolved parameter set.
    ///
    /// Reads exactly the parameters its card declares (invariant I-C3); a name the card
    /// does not declare cannot appear in a resolved set, because
    /// [`ParamSet::resolve`](v2xw_core::registry::ParamSet::resolve) rejects it.
    pub fn from_params(params: &ParamSet) -> Self {
        let defaults = WsmpParams::default();
        let max_wsm_payload_bytes = params
            .get_u64("max_wsm_payload_bytes")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(defaults.max_wsm_payload_bytes);
        Self::new(WsmpParams {
            max_wsm_payload_bytes,
        })
    }

    /// The parameters in force.
    pub const fn params(&self) -> &WsmpParams {
        &self.params
    }

    /// The largest WSM payload this layer accepts, bytes — the body of
    /// [`NetLayer::mtu`], callable without naming a context type.
    pub const fn payload_mtu(&self) -> u32 {
        self.params.max_wsm_payload_bytes
    }

    /// Octets the WSMP-T length field occupies for a payload of `payload_bytes`.
    ///
    /// The p-encoding rule (see the module documentation): one octet below 128 bytes, two up
    /// to [`WSMP_MAX_LENGTH_FIELD_PAYLOAD`]. A payload needing a third octet cannot fit the
    /// MSDU cap and is refused by [`NetLayer::encapsulate`].
    pub const fn length_field_bytes(payload_bytes: u32) -> u32 {
        match p_encoded_bytes(payload_bytes) {
            Some(n) => n,
            None => 4,
        }
    }

    /// The header size for `meta`, as a plain function — the body of
    /// [`NetLayer::header_bytes`], callable without naming a context type.
    pub const fn header_size(&self, meta: &NetMeta) -> u32 {
        let llc = if meta.include_llc_snap {
            LLC_SNAP_BYTES
        } else {
            0
        };
        WSMP_N_HEADER_BYTES
            + meta.extensions.bytes()
            + WSMP_TPID_BYTES
            + meta.psid.encoded_bytes()
            + Self::length_field_bytes(meta.sdu_bytes)
            + llc
    }
}

impl Model for WsmpNetLayer {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> NetLayer<C> for WsmpNetLayer
where
    C: Ctx + ?Sized,
{
    fn header_bytes(&self, meta: &NetMeta) -> u32 {
        self.header_size(meta)
    }

    fn encapsulate(&self, sdu: &[u8], meta: &NetMeta) -> Result<Vec<NetPdu>> {
        let sdu_bytes = u32::try_from(sdu.len()).unwrap_or(u32::MAX);
        let mtu = self.params.max_wsm_payload_bytes;
        if sdu_bytes > mtu {
            return Err(NetError::Oversize {
                layer: WSMP_NET_LAYER_ID,
                sdu_bytes,
                mtu,
            });
        }
        let needed = Self::length_field_bytes(sdu_bytes);
        if needed > 2 {
            return Err(NetError::PayloadTooLong {
                payload_bytes: sdu_bytes,
                needed,
                max: WSMP_MAX_LENGTH_FIELD_PAYLOAD,
            });
        }
        // The size is computed from the SDU actually offered, not from `meta.sdu_bytes`,
        // so a stale meta cannot make a PDU that claims a header it does not have.
        let meta = meta.with_sdu_bytes(sdu_bytes);
        Ok(vec![NetPdu {
            layer: WSMP_NET_LAYER_ID,
            header_bytes: self.header_size(&meta),
            transport: Transport::Wsmp { psid: meta.psid },
            gn: None,
            payload: sdu.to_vec(),
        }])
    }

    fn decapsulate(&mut self, _ctx: &mut C, _rx: NodeId, pdu: &NetPdu) -> DecapOutcome {
        match pdu.transport {
            Transport::Wsmp { psid } => DecapOutcome::Deliver {
                sdu: pdu.payload.clone(),
                transport: Transport::Wsmp { psid },
            },
            Transport::Btp { .. } => DecapOutcome::Drop(DropCause::MalformedPdu {
                detail: MalformedPdu::ForeignLayer,
            }),
        }
    }

    fn mtu(&self) -> u32 {
        self.payload_mtu()
    }
}

/// The model card for `net/wsmp/1609-3`.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        WSMP_NET_LAYER_ID,
        Family::Net,
        "1.0.0",
        "Exact IEEE 1609.3 WSMP v3 header sizes: the one-octet N-header, the optional \
         three-octet channel / data-rate / transmit-power extensions, the TPID octet, the \
         p-encoded PSID and the variable length field. Exposes the WSM payload cap and \
         fragments nothing.",
    );
    // Every tier counts headers; the sizes do not change with fidelity (04-models.md §7.5).
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "header size".to_string(),
            latex_or_text: "H = 1 + 3*k + 1 + p(PSID) + p(len)  [+ 8 if LLC/SNAP counted here]"
                .to_string(),
            notes: Some(
                "k is the number of present N-header extensions (0..3); p() is the IEEE \
                 1609.3 p-encoding width in octets. A BSM with PSID 0x20 and a payload of \
                 at least 128 B gives 1+0+1+1+2 = 5."
                    .to_string(),
            ),
        },
        Equation {
            name: "p-encoding width".to_string(),
            latex_or_text:
                "p(v) = 1 for v <= 0x7F, 2 for v <= 0x407F, 3 for v <= 0x20407F, 4 for v <= 0x1020407F"
                    .to_string(),
            notes: Some(
                "Prefix bits plus the previous range's exclusive end as an offset; the top \
                 of the four-octet range is the documented PSID maximum 0x1020407F."
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![Parameter {
        name: "max_wsm_payload_bytes".to_string(),
        unit: "B".to_string(),
        default: serde_json::json!(WSM_MAX_PAYLOAD_MIB_DEFAULT),
        range: Some(vec![
            serde_json::json!(1),
            serde_json::json!(WSM_MAX_PAYLOAD_SUPPORTED),
        ]),
        source: Source {
            kind: SourceKind::Standard,
            reference: "IEEE 1609.3-2020 MIB default maximum WSM payload (1,400 B); 2,302 B \
                        supported; MSDU cap 2,304 B"
                .to_string(),
            accessed: None,
            note: Some(
                "Quoted by USDOT CTI 4501 v01.01 §4.3.3.1.3.1 and by NDSS 2024; the 1609.3 \
                 clause itself is paywalled (R4 §F.1, VERIFIED secondary)."
                    .to_string(),
            ),
        },
        calibration: None,
    }];
    card.assumptions = vec![
        "Only the PSID-addressed WSMP transport forms are modelled. IEEE 1609.3 also offers \
         UDP/IPv6 and TCP/IPv6, which 04-models.md §7.1 puts outside Phases 1-2."
            .to_string(),
        "The TPID octet is counted once and its value is not modelled, because both \
         PSID-only forms cost the same single octet."
            .to_string(),
        "Header sizes are exact; the header's octets are not synthesised (see the crate \
         documentation). The SDU's bytes are the codec's own."
            .to_string(),
    ];
    card.limitations = vec![
        "The width of the WSMP-T length field is DERIVED. R4 §F.1 gives a one-octet length \
         in the 4-octet minimum and 04-models.md §7.1 gives a two-octet length for a BSM \
         with a payload of at least 128 B; the p-encoding boundary at 0x80 is the only rule \
         that yields both, and the standard's clause for it is not accessible."
            .to_string(),
        "The snippet \"minimum 5 bytes ... rarely exceeding 20\" attributed to the 1609.3 \
         text is UNVERIFIED and is not used for anything."
            .to_string(),
        "The two payload maxima and the MSDU cap are not mutually consistent. A 2,302 B \
         \"supported\" WSM payload plus even a minimum 4-octet WSMP header is 2,306 B, two \
         octets above the 2,304 B MSDU cap, so the figures come from different accounting \
         conventions (payload versus MSDU). The model enforces max_wsm_payload_bytes and \
         leaves the MSDU cap to the MAC model, which is where the frame is assembled \
         (04-models.md §4.6)."
            .to_string(),
        "WSMP does not fragment, so this layer refuses an oversize SDU rather than \
         splitting it: R4 §F.1 finds no fragmentation field in ShortMsgNpdu, ShortMsgData \
         is annotated \"maximum size is given by access technology\", and the Wireshark \
         dissector has no reassembly (VERIFIED by absence; the clause number is UNVERIFIED)."
            .to_string(),
    ];
    card.ignores = vec![
        "IEEE 1609.4 channel switching: the channel-number extension is sized, never acted \
         on."
        .to_string(),
        "WSA/WSM service advertisement semantics, which belong to the generator family \
         (04-models.md §8.1)."
            .to_string(),
    ];
    card.sources = vec![
        Source {
            kind: SourceKind::Code,
            reference: "Wireshark epan/dissectors/packet-wsmp.c (dissect_wsmp_v3), epan/etypes.h"
                .to_string(),
            accessed: Some("2026-09-17".to_string()),
            note: Some("Field-by-field layout and EtherType 0x88DC.".to_string()),
        },
        Source::new(
            SourceKind::Standard,
            "IEEE 1609.3 ASN.1 modules wsm.asn and wee.asn (ShortMsgNpdu, ShortMsgTpdus, \
             ShortMsgNextensions, VarLengthNumber)",
        ),
        Source::new(
            SourceKind::Standard,
            "USDOT CTI 4501 v01.01 §4.3.3.1.3.1 — WSM payload maxima",
        ),
        Source::new(
            SourceKind::Paper,
            "docs/design/research/R4-messages-envelopes.md §F.1 — the fact sheet these \
             sizes were extracted from",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §7.1 — the documented totals: minimum 4 B, BSM default 5 B, \
             BSM with all three N-header extensions 14 B",
        )],
        tests: vec![
            "wsmp::tests::the_default_bsm_header_is_five_bytes".to_string(),
            "wsmp::tests::every_documented_wsmp_total_is_reproduced".to_string(),
            "wsmp::tests::an_oversize_sdu_is_refused_because_wsmp_does_not_fragment".to_string(),
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
    use crate::netlayer::{BtpKind, BtpPort, Psid, VAR_LENGTH_NUMBER_MAX, WsmpExtensions};
    use crate::testctx::TestCtx;

    /// Calling through the trait needs a context type; the tests use the crate's own.
    fn layer() -> WsmpNetLayer {
        WsmpNetLayer::default()
    }

    fn header(l: &WsmpNetLayer, meta: &NetMeta) -> u32 {
        <WsmpNetLayer as NetLayer<TestCtx>>::header_bytes(l, meta)
    }

    /// 04-models.md §7.1: "Default `header_bytes` for a BSM: 5 (N-header 1 + TPID 1 +
    /// PSID 1 + length 2)."
    #[test]
    fn the_default_bsm_header_is_five_bytes() {
        let l = layer();
        // 180 B is Rostami 2018's digest-signed BSM SPDU (04-models.md §9.3), comfortably
        // above the 128 B boundary that makes the length field two octets.
        assert_eq!(header(&l, &NetMeta::for_bsm(180)), WSMP_BSM_HEADER_BYTES);
        assert_eq!(WSMP_BSM_HEADER_BYTES, 5);
        // Field by field.
        assert_eq!(
            WSMP_N_HEADER_BYTES + WSMP_TPID_BYTES + Psid::BSM.encoded_bytes() + 2,
            5
        );
    }

    /// Every total 04-models.md §7.1 states, reproduced by the same function.
    #[test]
    fn every_documented_wsmp_total_is_reproduced() {
        let l = layer();

        // Minimum: a payload below 128 B gives a one-octet length field.
        assert_eq!(
            header(&l, &NetMeta::for_bsm(127)),
            WSMP_MIN_HEADER_BYTES,
            "N-header 1 + TPID 1 + PSID 1 + length 1"
        );
        assert_eq!(
            header(&l, &NetMeta::for_bsm(128)),
            5,
            "the boundary at 0x80"
        );

        // With the three N-header extensions: 14.
        assert_eq!(
            header(
                &l,
                &NetMeta::for_bsm(180).with_extensions(WsmpExtensions::ALL)
            ),
            WSMP_BSM_HEADER_BYTES_WITH_ALL_EXTENSIONS
        );
        assert_eq!(WSMP_BSM_HEADER_BYTES_WITH_ALL_EXTENSIONS, 14);

        // Each extension is three octets, so the total is linear in their count.
        for (ext, extra) in [
            (WsmpExtensions::NONE, 0),
            (
                WsmpExtensions {
                    channel_number: true,
                    ..WsmpExtensions::NONE
                },
                3,
            ),
            (
                WsmpExtensions {
                    channel_number: true,
                    data_rate: true,
                    ..WsmpExtensions::NONE
                },
                6,
            ),
            (WsmpExtensions::ALL, 9),
        ] {
            assert_eq!(
                header(&l, &NetMeta::for_bsm(180).with_extensions(ext)),
                5 + extra
            );
        }

        // The PSID's own width, from one to four octets.
        for (psid, width) in [
            (0x20u32, 1),
            (0x80, 2),
            (0x4080, 3),
            (VAR_LENGTH_NUMBER_MAX, 4),
        ] {
            let meta = NetMeta::wsmp(180, Psid::new(psid).unwrap());
            assert_eq!(header(&l, &meta), 1 + 1 + width + 2, "PSID {psid:#x}");
        }
    }

    /// The LLC/SNAP flag is invariant I-N1 at the byte level: the eight octets below the
    /// network layer are counted here or by the MAC, never twice.
    #[test]
    fn llc_snap_is_counted_only_when_asked_for() {
        let l = layer();
        let meta = NetMeta::for_bsm(180);
        assert_eq!(header(&l, &meta), 5, "WSMP's documented figure excludes it");
        assert_eq!(header(&l, &meta.with_llc_snap(true)), 5 + LLC_SNAP_BYTES);
    }

    #[test]
    fn an_oversize_sdu_is_refused_because_wsmp_does_not_fragment() {
        let l = layer();
        assert_eq!(<WsmpNetLayer as NetLayer<TestCtx>>::mtu(&l), 1_400);
        assert!(!<WsmpNetLayer as NetLayer<TestCtx>>::fragments(&l));

        let sdu = vec![0u8; 1_401];
        let meta = NetMeta::for_bsm(1_401);
        assert_eq!(
            <WsmpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &sdu, &meta),
            Err(NetError::Oversize {
                layer: WSMP_NET_LAYER_ID,
                sdu_bytes: 1_401,
                mtu: 1_400,
            })
        );

        // At the supported maximum it is accepted, as one PDU.
        let l = WsmpNetLayer::new(WsmpParams {
            max_wsm_payload_bytes: WSM_MAX_PAYLOAD_SUPPORTED,
        });
        let sdu = vec![0u8; WSM_MAX_PAYLOAD_SUPPORTED as usize];
        let pdus =
            <WsmpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &sdu, &meta.with_sdu_bytes(2_302))
                .unwrap();
        assert_eq!(pdus.len(), 1, "WSMP never yields more than one PDU");
        assert_eq!(pdus[0].header_bytes, 5);
        assert_eq!(pdus[0].total_bytes(), 2_302 + 5);
        // …and that total is three octets ABOVE the 2,304 B MSDU cap, which is a
        // discrepancy between two secondary figures rather than a bug here: see the card's
        // limitation. The model enforces no relationship between them.
        assert!(pdus[0].total_bytes() > MAX_MSDU_BYTES);
        assert_eq!(
            WSM_MAX_PAYLOAD_SUPPORTED + WSMP_MIN_HEADER_BYTES,
            MAX_MSDU_BYTES + 2,
            "even a minimum WSMP header puts the 'supported' payload over the MSDU cap"
        );
    }

    #[test]
    fn the_pdu_records_the_header_the_sdu_actually_needs() {
        let l = layer();
        // A meta that claims 180 B, an SDU of 100: the PDU is sized from the SDU.
        let meta = NetMeta::for_bsm(180);
        let pdus =
            <WsmpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &[7u8; 100], &meta).unwrap();
        assert_eq!(
            pdus[0].header_bytes, 4,
            "a 100 B payload needs a 1 B length"
        );
        assert_eq!(pdus[0].sdu_bytes(), 100);
        assert_eq!(pdus[0].total_bytes(), 104);
        assert_eq!(pdus[0].layer, WSMP_NET_LAYER_ID);
        assert_eq!(pdus[0].gn, None);
    }

    #[test]
    fn decapsulation_delivers_the_sdu_and_rejects_a_foreign_pdu() {
        let mut l = layer();
        let mut ctx = TestCtx::new();
        let pdus = <WsmpNetLayer as NetLayer<TestCtx>>::encapsulate(
            &l,
            &[1u8, 2, 3, 4],
            &NetMeta::for_bsm(4),
        )
        .unwrap();
        let out = l.decapsulate(&mut ctx, NodeId::new(3), &pdus[0]);
        assert_eq!(
            out,
            DecapOutcome::Deliver {
                sdu: vec![1, 2, 3, 4],
                transport: Transport::Wsmp { psid: Psid::BSM },
            }
        );

        let foreign = NetPdu {
            layer: "net/gn-btp/en302636",
            header_bytes: 52,
            transport: Transport::Btp {
                kind: BtpKind::B,
                port: BtpPort::CAM,
            },
            gn: None,
            payload: vec![9, 9],
        };
        assert!(matches!(
            l.decapsulate(&mut ctx, NodeId::new(3), &foreign),
            DecapOutcome::Drop(DropCause::MalformedPdu { .. })
        ));
    }

    #[test]
    fn a_payload_too_long_for_the_length_field_is_refused() {
        // Above the two-octet p-encoding range. Only reachable with a parameter override
        // far above the MSDU cap, which is exactly why the check exists rather than an
        // assumption that it cannot happen.
        let l = WsmpNetLayer::new(WsmpParams {
            max_wsm_payload_bytes: 20_000,
        });
        let sdu = vec![0u8; 16_512];
        assert_eq!(
            <WsmpNetLayer as NetLayer<TestCtx>>::encapsulate(&l, &sdu, &NetMeta::for_bsm(16_512)),
            Err(NetError::PayloadTooLong {
                payload_bytes: 16_512,
                needed: 3,
                max: WSMP_MAX_LENGTH_FIELD_PAYLOAD,
            })
        );
    }

    #[test]
    fn the_card_validates_and_declares_no_rng() {
        let l = layer();
        let card = l.card();
        card.validate().expect("card validates");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Net);
        assert!(!card.determinism.uses_rng);
        assert_eq!(l.id(), WSMP_NET_LAYER_ID);
    }

    #[test]
    fn parameters_come_from_the_resolved_set() {
        let card = card();
        let params = ParamSet::resolve(&card, &serde_json::json!({"max_wsm_payload_bytes": 2302}))
            .expect("the override is declared and in range");
        let l = WsmpNetLayer::from_params(&params);
        assert_eq!(l.params().max_wsm_payload_bytes, 2_302);

        // The default survives an empty scenario section.
        let params = ParamSet::resolve(&card, &serde_json::Value::Null).unwrap();
        assert_eq!(
            WsmpNetLayer::from_params(&params)
                .params()
                .max_wsm_payload_bytes,
            WSM_MAX_PAYLOAD_MIB_DEFAULT
        );
    }
}
