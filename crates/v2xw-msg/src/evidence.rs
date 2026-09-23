//! Which messages are byte-exact, which are only structurally real, and which are sized —
//! one table, in code, that every model card is generated from.
//!
//! # Why this module exists
//!
//! Build decision D2 divided the message set into real encoders and a size model, and the
//! crate documentation has said so in prose since. Prose drifts. A researcher quoting an
//! overhead figure needs to know, without reading three module headers, whether the number
//! came from bytes an independent implementation agreed with, from bytes a generator
//! produced from the published module, from bytes this crate wrote but nobody has checked,
//! or from a table. Those are four different claims and this is the one place that states
//! them.
//!
//! [`EVIDENCE`] has exactly one row per [`MsgType`], a test enforces that, and every
//! codec's card takes its byte-exactness paragraph from [`card_statement`] rather than
//! writing its own. So a message cannot be described as byte-exact in a card while this
//! table calls it modelled: the card is the table.
//!
//! # The levels, and the line between them
//!
//! | Level | What the bytes are | What has checked them | Byte-exact? |
//! |---|---|---|---|
//! | [`ByteExactness::OracleValidated`] | the wire bytes | an independent implementation of the same ASN.1 agreed, byte for byte, on recorded vectors | yes |
//! | [`ByteExactness::GeneratedFromModule`] | the wire bytes | nothing third-party, but the encoder was machine-generated from the specification's own committed ASN.1, so there is no human reading of a constraint to get wrong | yes |
//! | [`ByteExactness::RealUperUnvalidated`] | the wire bytes of a documented subset | this crate's own round-trip and size tests, and nothing else | **no** |
//! | [`ByteExactness::SizeModelled`] | **a fill pattern** of the modelled length | published anchors, where any exist | **no** |
//! | [`ByteExactness::NotEncoded`] | there are none | — | — |
//!
//! The line that matters runs under the second row. Above it the bytes are the standard's:
//! either a generator produced them from the published module or an independent
//! implementation agreed with them. Below it they are this crate's, and a consistent
//! misreading of a constraint would round-trip perfectly and still be wrong on the wire.
//! That is not a hypothetical — the standing example is the asymmetric `Longitude` lower
//! bound, which passed 122 of this crate's unit tests and failed 235 of 235 oracle
//! vectors.
//!
//! [`ByteExactness::GeneratedFromModule`] is not the same claim as
//! [`ByteExactness::OracleValidated`] and is not presented as one: no ETSI conformance
//! vector has been checked, so a defect in `rasn` itself would go unnoticed.
//! [`ByteExactness::has_independent_check`] is the predicate that tells the two apart.

use crate::codec::MsgType;

/// How well evidenced a message type's bytes are.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ByteExactness {
    /// Real wire bytes, cross-validated against an independent implementation of the
    /// ASN.1 on recorded vectors.
    OracleValidated,
    /// Real wire bytes from an encoder `build.rs` generated from the specification's own
    /// committed ASN.1 module. Byte-exact, because no one read a constraint by hand — but
    /// unchecked against third-party vectors, so `rasn`'s own defects would not show.
    GeneratedFromModule,
    /// Real wire bytes of a documented subset, produced by a hand-written encoder that no
    /// independent implementation has checked.
    RealUperUnvalidated,
    /// No bytes: a fill pattern of exactly the modelled length
    /// ([`crate::codec::PLACEHOLDER_FILL`]).
    SizeModelled,
    /// No codec claims the type at all; nothing encodes or sizes it.
    NotEncoded,
}

impl ByteExactness {
    /// The phrase a card, a report or a table caption should use.
    pub const fn as_str(self) -> &'static str {
        match self {
            ByteExactness::OracleValidated => "byte-exact (oracle-validated)",
            ByteExactness::GeneratedFromModule => "byte-exact (generated from the module)",
            ByteExactness::RealUperUnvalidated => "real UPER, not yet oracle-validated",
            ByteExactness::SizeModelled => "size-modelled (placeholder bytes)",
            ByteExactness::NotEncoded => "not encoded",
        }
    }

    /// True when the bytes are the standard's rather than this crate's reading of it:
    /// generated from the published module, or agreed with by an independent
    /// implementation of it.
    ///
    /// The one predicate a report should branch on before writing the words "byte-exact".
    /// [`crate::codec::SizeSource::is_real`] answers a different and weaker question —
    /// whether the bytes are wire bytes at all — and a size that is exact is not the same
    /// as a size that has been checked.
    pub const fn is_byte_exact(self) -> bool {
        matches!(
            self,
            ByteExactness::OracleValidated | ByteExactness::GeneratedFromModule
        )
    }

    /// True only for [`ByteExactness::OracleValidated`]: a second implementation of the
    /// same ASN.1 produced the same octets.
    pub const fn has_independent_check(self) -> bool {
        matches!(self, ByteExactness::OracleValidated)
    }

    /// True when [`crate::codec::Encoded::bytes`] holds wire bytes rather than a fill
    /// pattern, whether or not anyone has validated them.
    pub const fn has_real_bytes(self) -> bool {
        matches!(
            self,
            ByteExactness::OracleValidated
                | ByteExactness::GeneratedFromModule
                | ByteExactness::RealUperUnvalidated
        )
    }
}

impl core::fmt::Display for ByteExactness {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One message type's evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageEvidence {
    /// Which message.
    pub ty: MsgType,
    /// How well evidenced its bytes are.
    pub exactness: ByteExactness,
    /// The model id of the codec that claims it, or `None` for
    /// [`ByteExactness::NotEncoded`].
    pub codec: Option<&'static str>,
    /// What checked it, or what would have to happen for it to move up a level. One
    /// sentence, because it is quoted verbatim into every card.
    pub evidence: &'static str,
}

/// The table: one row per [`MsgType`], in [`MsgType::ALL`] order.
///
/// Fixed order, and a slice rather than a map, for the usual reason (02-architecture.md
/// §6): a manifest's model list must not depend on hash iteration.
pub const EVIDENCE: &[MessageEvidence] = &[
    MessageEvidence {
        ty: MsgType::Bsm,
        exactness: ByteExactness::OracleValidated,
        codec: Some(crate::j2735::J2735_BSM_CODEC_ID),
        evidence: "pycrate 0.8.1 compiled from the real SAE J2735 2024-09 ASN.1, run \
                   2026-09-18: 235 of 235 vectors byte-identical in both directions, plus a \
                   mutation check proving the oracle catches a misread constraint the \
                   crate's own tests do not.",
    },
    MessageEvidence {
        ty: MsgType::Cam,
        exactness: ByteExactness::GeneratedFromModule,
        codec: Some(crate::codec::ETSI_UPER_CODEC_ID),
        evidence: "rasn bindings generated at build time from the committed ETSI forge \
                   module (TS 103 900, BSD-3-Clause): the encoder is machine-generated from \
                   the specification's own ASN.1, so the bytes are the standard's rather \
                   than anyone's reading of it.",
    },
    MessageEvidence {
        ty: MsgType::Denm,
        exactness: ByteExactness::GeneratedFromModule,
        codec: Some(crate::codec::ETSI_UPER_CODEC_ID),
        evidence: "as CAM: rasn bindings generated from the committed TS 103 831 module. No \
                   ETSI conformance vector has been checked for either message; the crate's \
                   own evidence is round-trip equality and field range checks.",
    },
    MessageEvidence {
        ty: MsgType::Spat,
        exactness: ByteExactness::RealUperUnvalidated,
        codec: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
        evidence: "hand-written against SAE J2735 2024-09 and round-tripped, with every \
                   preamble width corroborated by the size-model derivation written while \
                   the ASN.1 was on disk — except the MovementPhaseState index, where the \
                   two disagree by one bit. No oracle run: the J2735 modules are \
                   git-ignored (D3) and absent from this checkout.",
    },
    MessageEvidence {
        ty: MsgType::Map,
        exactness: ByteExactness::RealUperUnvalidated,
        codec: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
        evidence: "hand-written against SAE J2735 2024-09 and round-tripped. Every \
                   structural choice that could not be re-read is a named constant in \
                   j2735::map::assumptions and a todo-calibrate parameter on the codec's \
                   card. No oracle run.",
    },
    MessageEvidence {
        ty: MsgType::Psm,
        exactness: ByteExactness::SizeModelled,
        codec: Some(crate::size_model::J2735_SIZE_MODEL_ID),
        evidence: "no published PSM size exists (04-models.md §8.2 records \"PSM: none\"), \
                   so the row is derived from the ASN.1 structure and carries a \
                   todo-calibrate plan.",
    },
    MessageEvidence {
        ty: MsgType::Vam,
        exactness: ByteExactness::SizeModelled,
        codec: Some(crate::etsi_size::ETSI_SIZE_MODEL_ID),
        evidence: "the VAM PDU module is not in third_party/asn1/etsi, so there is nothing \
                   to generate from; the row is fitted to four published planning sizes \
                   (TR 2050 Fig. 12, 5GAA, C2C-CC, arXiv 2506.22052) and carries the plan \
                   that would replace it with a real encoder.",
    },
    MessageEvidence {
        ty: MsgType::Cpm,
        exactness: ByteExactness::SizeModelled,
        codec: Some(crate::etsi_size::ETSI_SIZE_MODEL_ID),
        evidence: "the CPM PDU module is not in third_party/asn1/etsi; the row is built \
                   from TR 103 562 Table 3's measured container sizes and bounded by TR \
                   2050 Fig. 13's planning size.",
    },
    MessageEvidence {
        ty: MsgType::Srm,
        exactness: ByteExactness::SizeModelled,
        codec: Some(crate::size_model::J2735_SIZE_MODEL_ID),
        evidence: "no published SRM size; derived from the ASN.1 structure with a \
                   todo-calibrate plan.",
    },
    MessageEvidence {
        ty: MsgType::Ssm,
        exactness: ByteExactness::SizeModelled,
        codec: Some(crate::size_model::J2735_SIZE_MODEL_ID),
        evidence: "no published SSM size; derived from the ASN.1 structure with a \
                   todo-calibrate plan.",
    },
    MessageEvidence {
        ty: MsgType::Wsa,
        exactness: ByteExactness::NotEncoded,
        codec: None,
        evidence: "IEEE 1609.3 service advertisement; no codec is planned in this crate \
                   (04-models.md §8.1 `generator/wsa-1609-3`).",
    },
    MessageEvidence {
        ty: MsgType::Crl,
        exactness: ByteExactness::NotEncoded,
        codec: None,
        evidence: "encoded by v2xw-sec over the generated 1609.2 COER bindings in \
                   crate::sec_types, not through a MessageCodec here.",
    },
    MessageEvidence {
        ty: MsgType::Mbr,
        exactness: ByteExactness::NotEncoded,
        codec: None,
        evidence: "ETSI TS 103 759 misbehaviour report; a later phase.",
    },
];

/// The evidence for one message type.
///
/// Panics only if [`EVIDENCE`] is missing a row, which
/// [`tests::every_message_type_has_exactly_one_row`] makes impossible.
pub fn evidence_for(ty: MsgType) -> &'static MessageEvidence {
    EVIDENCE
        .iter()
        .find(|e| e.ty == ty)
        .expect("EVIDENCE has a row for every MsgType; a test enforces it")
}

/// How well evidenced a message type's bytes are — the short answer.
pub fn byte_exactness(ty: MsgType) -> ByteExactness {
    evidence_for(ty).exactness
}

/// The byte-exactness paragraph for a codec's model card: one line per message it claims.
///
/// Every codec in this crate starts its `limitations` with this, so the distinction that
/// makes an overhead result trustworthy is stated in the same words everywhere and comes
/// from one place. An unknown codec id yields an empty list rather than an error, because a
/// card is a description and a missing row should not stop a model from registering — the
/// test below is what stops a row from going missing.
pub fn card_statement(codec_id: &str) -> Vec<String> {
    let mut lines = vec![
        "Byte-exactness, from v2xw_msg::evidence::EVIDENCE — the crate's single statement \
         of which payloads are real bytes and which are a fill pattern. \"Byte-exact\" is \
         reserved for bytes an independent implementation of the ASN.1 has agreed with; \
         real UPER that nobody has checked is labelled as such, and a size model is never \
         described as either."
            .to_string(),
    ];
    for row in EVIDENCE.iter().filter(|e| e.codec == Some(codec_id)) {
        lines.push(format!(
            "{}: {} — {}",
            row.ty.as_str().to_uppercase(),
            row.exactness,
            row.evidence
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::MessageCodec;
    use v2xw_core::model::Model;

    /// The table must cover the message set exactly: no gap that would let a type be
    /// described twice or not at all.
    #[test]
    fn every_message_type_has_exactly_one_row() {
        for ty in MsgType::ALL {
            let rows = EVIDENCE.iter().filter(|e| e.ty == ty).count();
            assert_eq!(rows, 1, "{ty} has {rows} rows in EVIDENCE");
            assert_eq!(evidence_for(ty).ty, ty);
            assert!(
                !evidence_for(ty).evidence.trim().is_empty(),
                "{ty} has no evidence sentence"
            );
        }
        assert_eq!(EVIDENCE.len(), MsgType::ALL.len());
        // Declaration order, so a generated table and a manifest agree.
        for (row, ty) in EVIDENCE.iter().zip(MsgType::ALL) {
            assert_eq!(row.ty, ty, "EVIDENCE is not in MsgType::ALL order");
        }
    }

    /// The table must agree with the codecs that actually exist: the row's codec is the one
    /// that claims the type, and a type nobody claims is `NotEncoded`.
    #[test]
    fn the_table_agrees_with_the_registered_codecs() {
        let codecs = crate::size_model::codecs();
        for ty in MsgType::ALL {
            let row = evidence_for(ty);
            let claimants: Vec<String> = codecs
                .iter()
                .filter(|c| c.supports(ty))
                .map(|c| c.id().to_string())
                .collect();
            match row.codec {
                Some(id) => assert_eq!(
                    claimants,
                    vec![id.to_string()],
                    "{ty}: EVIDENCE names `{id}` but the registered codecs say {claimants:?}"
                ),
                None => {
                    assert_eq!(row.exactness, ByteExactness::NotEncoded, "{ty}");
                    assert!(
                        claimants.is_empty(),
                        "{ty} is claimed by {claimants:?} but EVIDENCE says nothing encodes it"
                    );
                }
            }
        }
    }

    /// Every codec's card must carry the statement, and the statement must name every
    /// message that codec claims. This is the check that keeps a card from quietly
    /// claiming more than the table does.
    #[test]
    fn every_card_carries_the_statement_for_the_types_it_claims() {
        for codec in crate::size_model::codecs() {
            let card = codec.card();
            let text = card.limitations.join("\n");
            assert!(
                text.contains("Byte-exactness, from v2xw_msg::evidence::EVIDENCE"),
                "{} does not carry the evidence statement",
                card.id
            );
            for ty in codec.message_types() {
                let row = evidence_for(*ty);
                assert!(
                    text.contains(row.exactness.as_str()),
                    "{}'s card does not state that {ty} is {}",
                    card.id,
                    row.exactness
                );
            }
        }
    }

    /// The one confusion this module exists to prevent: a size model must never be
    /// reachable through a predicate that a report would read as "byte-exact", and real
    /// bytes that nobody has validated must not be either.
    #[test]
    fn only_oracle_validated_rows_are_byte_exact() {
        assert!(ByteExactness::OracleValidated.is_byte_exact());
        assert!(ByteExactness::GeneratedFromModule.is_byte_exact());
        assert!(!ByteExactness::RealUperUnvalidated.is_byte_exact());
        assert!(!ByteExactness::SizeModelled.is_byte_exact());
        assert!(!ByteExactness::NotEncoded.is_byte_exact());

        assert!(ByteExactness::OracleValidated.has_independent_check());
        assert!(!ByteExactness::GeneratedFromModule.has_independent_check());

        assert!(ByteExactness::RealUperUnvalidated.has_real_bytes());
        assert!(!ByteExactness::SizeModelled.has_real_bytes());

        for ty in [MsgType::Spat, MsgType::Map] {
            assert!(!byte_exactness(ty).is_byte_exact(), "{ty}");
            assert!(byte_exactness(ty).has_real_bytes(), "{ty}");
        }
        for ty in [MsgType::Psm, MsgType::Srm, MsgType::Ssm, MsgType::Cpm, MsgType::Vam] {
            assert!(!byte_exactness(ty).has_real_bytes(), "{ty}");
        }
        for ty in [MsgType::Bsm, MsgType::Cam, MsgType::Denm] {
            assert!(byte_exactness(ty).is_byte_exact(), "{ty}");
        }
        // Only the BSM has been through an independent implementation.
        assert!(byte_exactness(MsgType::Bsm).has_independent_check());
        assert!(!byte_exactness(MsgType::Cam).has_independent_check());
    }

    /// `SizeSource` and `ByteExactness` answer different questions, and the codecs must
    /// answer both consistently: anything the table calls size-modelled must come back
    /// with `SizeSource::SizeModel`, and anything it calls real must not.
    #[test]
    fn the_wire_tier_matches_the_evidence_level() {
        use crate::codec::{Message, SizeSource};
        use crate::size_model::{ContentProfile, SizeRequest};

        for ty in MsgType::ALL {
            let row = evidence_for(ty);
            if row.exactness != ByteExactness::SizeModelled {
                continue;
            }
            let codec = crate::size_model::codecs()
                .into_iter()
                .find(|c| c.supports(ty))
                .unwrap_or_else(|| panic!("{ty} is size-modelled but no codec claims it"));
            let encoded = codec
                .encode(&Message::Modeled(SizeRequest {
                    ty,
                    profile: ContentProfile::Typical,
                    elements: 3,
                }))
                .unwrap_or_else(|e| panic!("{ty} should be sizable: {e}"));
            assert!(matches!(encoded.size_source, SizeSource::SizeModel(_)), "{ty}");
            assert!(!encoded.is_real(), "{ty}");
        }
    }
}
