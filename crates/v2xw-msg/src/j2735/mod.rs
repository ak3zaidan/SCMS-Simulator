//! SAE J2735: hand-written UPER codecs for the messages the simulator puts on the wire.
//!
//! The middle tier of build decision D2. The ETSI stack is generated from committed
//! BSD-3-Clause modules and the rest of J2735 is a size model; these messages sit between
//! them, with genuinely real bytes produced by code written against the 2024-09 ASN.1
//! rather than by a code generator that cannot compile it.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`uper`] | the bit-level engine: a bit writer and reader, and one function per X.691 construct these messages need |
//! | [`bsm`] | the Basic Safety Message: `BSMcoreData` field for field, the `VehicleSafetyExtensions` Part II container, the `MessageFrame` wrapper, and the builder from a node's belief |
//! | [`spat`] | `SPAT`: intersection states, movement states, movement events and their timing |
//! | [`map`] | `MapData`: intersection geometry, lanes, node lists and connections |
//! | [`infra`] | the [`crate::MessageCodec`] seam for SPaT and MAP, and the card that states how well evidenced they are |
//!
//! # Trusting the bytes — and the two codecs are not equally trustworthy
//!
//! A hand-written encoder that only ever round-trips against itself proves nothing: a
//! consistent misreading of a constraint — the asymmetric `Longitude` lower bound is the
//! standing example — round-trips perfectly and is wrong on the wire. So the evidence for
//! the BSM codec is an **oracle**: `tests/j2735_oracle.rs` generates pseudo-random but
//! valid BSMs from a fixed seed, encodes them here, and has `pycrate` (which compiles the
//! real ASN.1 at run time) decode them and compare field for field — then encode from the
//! same field values and compare bytes, and finally hand its own bytes back for this codec
//! to decode. Three directions, because each catches a different class of defect. That run
//! happened: 235 vectors, byte-identical.
//!
//! **The SPaT and MAP codecs have had no such run.** `third_party/asn1/j2735/` is
//! git-ignored (build decision D3) and is absent from this checkout, and the oracle's
//! Python environment is not on this machine either, so their ASN.1 could not even be
//! re-read while they were written. `tests/j2735_infra_oracle.rs` holds their harness and
//! skips with a message saying so. Until it runs, [`crate::evidence`] calls their bytes
//! *real UPER, not yet oracle-validated*, which is a weaker claim than the BSM's and is
//! never to be reported as the same one.
//!
//! The BSM oracle skips the same way when its environment is absent, which is how it
//! behaves in CI; it is not skipped when the environment is present, and the codec's model
//! card records the pass count from the run that validated it.
//!
//! # Where the message type is claimed
//!
//! [`J2735BsmCodec`] is the [`crate::MessageCodec`] for [`crate::MsgType::Bsm`], and
//! [`crate::size_model::codecs`] includes it, so the registry's
//! [`crate::size_model::assert_no_overlapping_claims`] check covers it like any other.
//!
//! Note what the codec seam can and cannot carry. [`crate::Message`] has no structured BSM
//! variant — a BSM travels as [`crate::Message::HandEncoded`] — so [`J2735BsmCodec::encode`]
//! takes bytes and *validates* them by decoding, while [`bsm::encode_bsm`] and
//! [`bsm::build_bsm`] are the structured path a node actually uses. That is deliberate:
//! putting a 14-field wire structure into the engine-wide `Message` enum would make every
//! consumer of that enum depend on J2735's field layout.

pub mod bsm;
pub mod infra;
pub mod map;
pub mod spat;
pub mod uper;

pub use infra::{J2735_INFRA_CODEC_ID, J2735InfraCodec};

use v2xw_core::card::{Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus};
use v2xw_core::model::Model;

use crate::codec::{Encoded, Message, MessageCodec, MsgType};
use crate::error::CodecError;

/// Model id of the hand-written J2735 BSM codec.
pub const J2735_BSM_CODEC_ID: &str = "codec/uper/j2735-bsm";

/// The hand-written J2735 BSM codec.
///
/// Stateless; one instance serves every node, and building the card is its only cost.
#[derive(Debug, Clone)]
pub struct J2735BsmCodec {
    card: ModelCard,
}

impl Default for J2735BsmCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl J2735BsmCodec {
    /// The one type this codec implements.
    pub const TYPES: [MsgType; 1] = [MsgType::Bsm];

    /// Builds the codec and its card.
    pub fn new() -> Self {
        Self { card: card() }
    }
}

impl Model for J2735BsmCodec {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MessageCodec for J2735BsmCodec {
    fn message_types(&self) -> &[MsgType] {
        &Self::TYPES
    }

    /// Validates an already-encoded BSM and hands the bytes back.
    ///
    /// The [`Message`] enum carries a BSM as bytes ([`Message::HandEncoded`]), so there is
    /// nothing to encode here — but there *is* something to check, and this checks it: the
    /// bytes are decoded, which enforces every constraint, every enumeration index and the
    /// absence of trailing data, and are then re-encoded and compared. Equal bytes prove
    /// the payload is in the canonical form this codec produces; unequal bytes are refused
    /// with the difference named, because a receiver that re-encoded to sign or hash would
    /// otherwise get a different digest from the sender.
    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError> {
        match msg {
            Message::HandEncoded {
                ty: MsgType::Bsm,
                bytes,
            } => {
                let decoded = bsm::decode_bsm(bytes)?;
                let re = bsm::encode_bsm(&decoded)?;
                if re.bytes != *bytes {
                    return Err(CodecError::Encode {
                        ty: MsgType::Bsm,
                        detail: format!(
                            "the payload decodes but is not in canonical UPER form: \
                             {} octets in, {} octets out",
                            bytes.len(),
                            re.bytes.len()
                        ),
                    });
                }
                Ok(re)
            }
            other => Err(CodecError::Unsupported {
                codec: J2735_BSM_CODEC_ID.to_string(),
                ty: other.msg_type(),
            }),
        }
    }

    /// Decodes a BSM and returns it as [`Message::HandEncoded`].
    ///
    /// The bytes come back rather than a structure, for the same reason [`Self::encode`]
    /// takes them: the engine-wide `Message` enum has no J2735 variant. What the caller
    /// gains over keeping its own bytes is the guarantee that they decode —
    /// [`bsm::decode_bsm`] is the structured reader for anything that needs the fields.
    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError> {
        if t != MsgType::Bsm {
            return Err(CodecError::Unsupported {
                codec: J2735_BSM_CODEC_ID.to_string(),
                ty: t,
            });
        }
        bsm::decode_bsm(bytes)?;
        Ok(Message::HandEncoded {
            ty: MsgType::Bsm,
            bytes: bytes.to_vec(),
        })
    }
}

/// The card, which is where "which fields are real" is recorded for the run manifest.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        J2735_BSM_CODEC_ID,
        Family::Codec,
        "1.0.0",
        "Hand-written ASN.1 UPER codec for the SAE J2735 Basic Safety Message: BSMcoreData \
         in full, the VehicleSafetyExtensions Part II container, and the MessageFrame \
         wrapper. Cross-validated against a pycrate oracle compiled from the real ASN.1.",
    );
    // A codec has no cheap variant: the abstract tier still needs an exact size, and this
    // one produces real bytes for free.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];

    card.assumptions = vec![
        "The message set is SAE J2735 2024-09 (BasicSafetyMessage v1.1.2 over Common \
         v1.1.2). Earlier editions differ in ways that matter: the 2016 Longitude range is \
         symmetric, and VehicleEventFlags gained its 14th bit and its size extension marker \
         after 2016."
            .to_string(),
        "secMark is filled with the millisecond within the current UTC minute, derived from \
         the scenario wall clock, so it ignores leap seconds."
            .to_string(),
        "A Part II container the codec does not model is carried through as the open type's \
         octets and re-encoded verbatim, which preserves it exactly without interpreting it."
            .to_string(),
    ];

    // The byte-exactness statement, from the crate's one evidence table.
    card.limitations = crate::evidence::card_statement(J2735_BSM_CODEC_ID);
    card.limitations.extend([
        // The fields that are real, named exhaustively. A card that said "most of Part I" \
        // would be useless to anyone auditing a run.
        "Real fields — BSMcoreData: msgCnt, id, secMark, lat, long, elev, \
         accuracy(semiMajor, semiMinor, orientation), transmission, speed, heading, angle, \
         accelSet(long, lat, vert, yaw), brakes(wheelBrakes, traction, abs, scs, \
         brakeBoost, auxBrakes), size(width, length). All 14 fields, all mandatory, 290 \
         bits."
            .to_string(),
        "Real fields — VehicleSafetyExtensions (Part II id 0): events (13-bit root), \
         pathHistory(currGNSSstatus, crumbData of 1..23 PathHistoryPoint, each with \
         latOffset, lonOffset, elevationOffset, timeOffset and optional speed, posAccuracy \
         and heading), pathPrediction(radiusOfCurve, confidence), lights (9-bit root)."
            .to_string(),
        "Unimplemented — PathHistory.initialPosition (FullPositionVector). Refused with \
         CodecError::UnsupportedConstruct on decode; never emitted."
            .to_string(),
        "Unimplemented — BasicSafetyMessage.regional (RegionalExtension over \
         Reg-BasicSafetyMessage). Refused on decode; never emitted."
            .to_string(),
        "Unimplemented — SpecialVehicleExtensions (Part II id 1) and \
         SupplementalVehicleExtensions (Part II id 2) as structures. Carried opaquely: \
         decoded to bsm::PartIIValue::Opaque and re-encoded byte for byte, but their \
         fields are not readable."
            .to_string(),
        "Unimplemented — extension additions in any SEQUENCE, and bit strings longer than \
         their size constraint's extension root (VehicleEventFlags eventJackKnife, \
         ExteriorLights beyond 9 bits). All refused rather than skipped, because skipping \
         bits in a PER encoding desynchronises every field after them."
            .to_string(),
        "YawRate declares no unavailable value, so a missing yaw rate is indistinguishable \
         from a genuine zero. That is a property of the standard, not of this codec."
            .to_string(),
    ]);

    card.ignores = vec![
        "Every other J2735 message. SPaT and MAP are hand-encoded by \
         codec/uper/j2735-spat-map, whose bytes are real but not yet oracle-validated; \
         PSM, SRM and SSM are sized by codec/size-model/j2735 and carry placeholder bytes."
            .to_string(),
        "The IEEE 1609.2 security envelope, which v2xw-sec puts around this payload over \
         the COER bindings in crate::sec_types."
            .to_string(),
    ];

    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "SAE J2735 2024-09 — BasicSafetyMessage-2024-rel-v1.1.2 and Common-2024-rel-v1.1.2 \
             (read locally; never redistributed, per build decision D3)",
        ),
        Source::new(
            SourceKind::Standard,
            "SAE J2735 2024-09 — MessageFrame-2024-rel-v1.1.1, DSRCmsgID basicSafetyMessage(20)",
        ),
        Source::new(
            SourceKind::Standard,
            "ITU-T X.691 (02/2021) — Packed Encoding Rules, UNALIGNED variant: clauses 11.2, \
             11.5, 11.9, 12, 14, 16, 17, 19",
        ),
        Source::new(
            SourceKind::Code,
            "pycrate 0.8.1 — the independent ASN.1 implementation the oracle test compares \
             against, compiling all 43 J2735 2024-09 modules at run time",
        ),
        Source::new(
            SourceKind::Code,
            "this repository: docs/design/12-build-decisions.md D2 (encoder strategy), D3 \
             (J2735 modules are never committed), D9 (writer-side quantisation)",
        ),
    ];

    card.validation = Validation {
        // The core crate's ValidationStatus has no `cross-validated` level, and this crate
        // may not add one. `LiteratureChecked` is the closest available and understates
        // what was done: the comparison is against an independent implementation of the
        // standard, not against a published table. The reference below says so exactly.
        status: ValidationStatus::LiteratureChecked,
        references: vec![
            Source::new(
                SourceKind::Code,
                "pycrate 0.8.1 oracle over the real SAE J2735 2024-09 ASN.1, run 2026-09-18: \
                 235 vectors (35 boundary vectors covering the minimum, maximum and \
                 unavailable value of every Part I field plus the smallest and largest Part \
                 II container, and 200 pseudo-random vectors from a fixed seed). 235/235 \
                 byte-identical encodings of the BasicSafetyMessage PDU and of its \
                 MessageFrame; 235/235 field-identical decodings in both directions; 2 \
                 pycrate-built messages carrying unmodelled Part II containers re-encoded \
                 byte for byte.",
            ),
            Source::new(
                SourceKind::Code,
                "Mutation check of the oracle itself, same run: reading Longitude's lower \
                 bound as symmetric (-1800000000) passes all 122 of this crate's own unit \
                 tests and is caught by the oracle on 235 of 235 vectors; dropping \
                 PathPrediction's extension bit likewise passes all 122 and fails 52 \
                 vectors. The oracle is therefore testing something the crate cannot test \
                 by itself.",
            ),
        ],
        tests: vec![
            "j2735::bsm::tests::a_part_i_bsm_is_thirty_seven_octets".to_string(),
            "j2735::bsm::tests::the_extremes_of_every_field_round_trip".to_string(),
            "j2735::bsm::tests::part_ii_vehicle_safety_round_trips".to_string(),
            "j2735::bsm::tests::an_opaque_part_ii_container_survives_byte_for_byte".to_string(),
            "j2735::uper::tests::a_constrained_width_is_the_bits_needed_for_the_range".to_string(),
            "j2735_oracle::rust_encodings_match_pycrate_byte_for_byte".to_string(),
            "j2735_oracle::pycrate_encodings_decode_field_for_field".to_string(),
            "j2735_oracle::unmodelled_part_ii_containers_survive_pycrate_round_trip".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::SizeSource;

    fn payload() -> Vec<u8> {
        bsm::encode_bsm(&bsm::BasicSafetyMessage::part_i(
            bsm::BsmCoreData::unavailable([1, 2, 3, 4]),
        ))
        .expect("encodes")
        .bytes
    }

    #[test]
    fn the_card_validates_and_names_the_family() {
        let codec = J2735BsmCodec::new();
        codec.card().validate().expect("card validates");
        codec.card().check_api_version().expect("api version");
        assert_eq!(codec.family(), Family::Codec);
        assert_eq!(codec.id(), J2735_BSM_CODEC_ID);
    }

    /// The card is the run manifest's only account of which fields are real, so a silent
    /// deletion of one of those lines is a defect in its own right.
    #[test]
    fn the_card_names_both_the_real_and_the_unimplemented_fields() {
        let card = J2735BsmCodec::new().card().clone();
        let text = card.limitations.join("\n");
        for field in [
            "msgCnt",
            "secMark",
            "accelSet",
            "wheelBrakes",
            "pathPrediction",
            "initialPosition",
            "regional",
            "YawRate",
        ] {
            assert!(text.contains(field), "the card should account for {field}");
        }
        assert!(
            card.limitations
                .iter()
                .any(|l| l.starts_with("Real fields")),
            "the real fields must be listed, not just the missing ones"
        );
    }

    #[test]
    fn it_claims_exactly_the_bsm() {
        let codec = J2735BsmCodec::new();
        assert!(codec.supports(MsgType::Bsm));
        for ty in MsgType::ALL {
            if ty != MsgType::Bsm {
                assert!(!codec.supports(ty), "{ty} should not be claimed");
            }
        }
    }

    #[test]
    fn the_seam_validates_bytes_and_returns_them_unchanged() {
        let codec = J2735BsmCodec::new();
        let bytes = payload();
        let encoded = codec
            .encode(&Message::HandEncoded {
                ty: MsgType::Bsm,
                bytes: bytes.clone(),
            })
            .expect("valid payload");
        assert_eq!(encoded.bytes, bytes);
        assert_eq!(encoded.size_source, SizeSource::Uper);

        let Message::HandEncoded { ty, bytes: back } =
            codec.decode(&bytes, MsgType::Bsm).expect("decodes")
        else {
            unreachable!("the BSM codec returns HandEncoded")
        };
        assert_eq!(ty, MsgType::Bsm);
        assert_eq!(back, bytes);
    }

    #[test]
    fn the_seam_refuses_bytes_that_are_not_a_bsm() {
        let codec = J2735BsmCodec::new();
        let err = codec
            .encode(&Message::HandEncoded {
                ty: MsgType::Bsm,
                bytes: vec![0xff; 4],
            })
            .expect_err("four random octets are not a BSM");
        assert!(err.to_string().contains("bsm"), "{err}");
        assert!(codec.decode(&[0xa5; 37], MsgType::Bsm).is_err());
    }

    #[test]
    fn the_seam_refuses_another_codec_s_message() {
        let codec = J2735BsmCodec::new();
        let err = codec
            .encode(&Message::HandEncoded {
                ty: MsgType::Cam,
                bytes: vec![0; 10],
            })
            .expect_err("not ours");
        assert!(matches!(err, CodecError::Unsupported { .. }));
        assert!(codec.decode(&payload(), MsgType::Cam).is_err());
    }

    #[test]
    fn a_codec_is_usable_as_a_trait_object() {
        let codec: Box<dyn MessageCodec> = Box::new(J2735BsmCodec::new());
        assert_eq!(codec.message_types(), &[MsgType::Bsm]);
    }
}
