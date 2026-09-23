//! The [`crate::MessageCodec`] seam for the hand-written J2735 infrastructure messages.
//!
//! [`J2735InfraCodec`] claims [`MsgType::Spat`] and [`MsgType::Map`] and routes them to
//! [`crate::j2735::spat`] and [`crate::j2735::map`]. It is a separate codec from
//! [`crate::j2735::J2735BsmCodec`] for one reason, and it is the reason this module has a
//! long card: **the evidence behind its bytes is weaker**, and a registry entry is where a
//! run's manifest goes to find that out. Merging the two would average an oracle-validated
//! encoder together with an unvalidated one behind a single status.
//!
//! Like the BSM codec, this one carries messages as [`Message::HandEncoded`] bytes rather
//! than as a structured [`Message`] variant: putting a MAP's lane geometry into the
//! engine-wide message enum would make every consumer of that enum depend on J2735's field
//! layout. [`crate::j2735::spat::encode_spat`] and [`crate::j2735::map::encode_map`] are
//! the structured path.

use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;

use crate::codec::{Encoded, Message, MessageCodec, MsgType};
use crate::error::CodecError;
use crate::j2735::{map, spat};

/// Model id of the hand-written J2735 SPaT and MAP codec.
pub const J2735_INFRA_CODEC_ID: &str = "codec/uper/j2735-spat-map";

/// The hand-written J2735 SPaT and MAP codec.
///
/// Stateless; one instance serves every RSU, and building the card is its only cost.
#[derive(Debug, Clone)]
pub struct J2735InfraCodec {
    card: ModelCard,
}

impl Default for J2735InfraCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl J2735InfraCodec {
    /// The two types this codec implements.
    pub const TYPES: [MsgType; 2] = [MsgType::Spat, MsgType::Map];

    /// Builds the codec and its card.
    pub fn new() -> Self {
        Self { card: card() }
    }
}

impl Model for J2735InfraCodec {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MessageCodec for J2735InfraCodec {
    fn message_types(&self) -> &[MsgType] {
        &Self::TYPES
    }

    /// Validates already-encoded bytes and hands them back.
    ///
    /// The same contract as [`crate::j2735::J2735BsmCodec::encode`]: the bytes are decoded,
    /// which enforces every constraint and every refusal, then re-encoded and compared. A
    /// difference means the payload is not in the canonical form this codec produces, and
    /// is refused rather than passed on — a receiver that re-encoded to sign or hash would
    /// otherwise compute a different digest from the sender's.
    ///
    /// For a MAP, "canonical" includes each node's `NodeOffsetPointXY` alternative, which
    /// [`map::XyOffset`] keeps as part of the value precisely so that this comparison can
    /// succeed for an encoder that chose a wider alternative than strictly necessary.
    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError> {
        let (ty, bytes) = match msg {
            Message::HandEncoded { ty, bytes } if Self::TYPES.contains(ty) => (*ty, bytes),
            other => {
                return Err(CodecError::Unsupported {
                    codec: J2735_INFRA_CODEC_ID.to_string(),
                    ty: other.msg_type(),
                });
            }
        };
        let re = match ty {
            MsgType::Spat => spat::encode_spat(&spat::decode_spat(bytes)?)?,
            MsgType::Map => map::encode_map(&map::decode_map(bytes)?)?,
            // Unreachable: the match above admits only the two types in `TYPES`. If a
            // third is ever added there without a branch here, this refuses it by name
            // rather than encoding it as one of the other two.
            other => {
                return Err(CodecError::Unsupported {
                    codec: J2735_INFRA_CODEC_ID.to_string(),
                    ty: other,
                });
            }
        };
        if re.bytes != *bytes {
            return Err(CodecError::Encode {
                ty,
                detail: format!(
                    "the payload decodes but is not in canonical UPER form: {} octets in, \
                     {} octets out",
                    bytes.len(),
                    re.bytes.len()
                ),
            });
        }
        Ok(re)
    }

    /// Decodes and returns the bytes as [`Message::HandEncoded`].
    ///
    /// What the caller gains over keeping its own bytes is the guarantee that they decode:
    /// [`spat::decode_spat`] and [`map::decode_map`] are the structured readers.
    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError> {
        match t {
            MsgType::Spat => {
                spat::decode_spat(bytes)?;
            }
            MsgType::Map => {
                map::decode_map(bytes)?;
            }
            other => {
                return Err(CodecError::Unsupported {
                    codec: J2735_INFRA_CODEC_ID.to_string(),
                    ty: other,
                });
            }
        }
        Ok(Message::HandEncoded {
            ty: t,
            bytes: bytes.to_vec(),
        })
    }
}

/// The card: what is real, what is refused, and what is not yet proven.
fn card() -> ModelCard {
    let mut card = ModelCard::new(
        J2735_INFRA_CODEC_ID,
        Family::Codec,
        "1.0.0",
        "Hand-written ASN.1 UPER codec for the SAE J2735 SPAT and MapData messages, over \
         the subset an intersection application fills. Real wire bytes, measured sizes, \
         and — unlike the BSM codec — NOT yet cross-validated against an independent \
         implementation of the ASN.1.",
    );
    // A codec has no cheap variant: the abstract tier still needs an exact size.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];

    // The byte-exactness statement, generated from the crate's one evidence table so the
    // card and the table cannot drift apart.
    card.limitations = crate::evidence::card_statement(J2735_INFRA_CODEC_ID);

    card.limitations.push(
        "Real fields — SPAT: timeStamp; per IntersectionState id (region and id), revision, \
         status (16-bit IntersectionStatusObject), moy, timeStamp, and 1..255 MovementState, \
         each with signalGroup and 1..16 MovementEvent carrying eventState \
         (MovementPhaseState) and an optional TimeChangeDetails (startTime, minEndTime, \
         maxEndTime, likelyTime, confidence, nextTime)."
            .to_string(),
    );
    card.limitations.push(
        "Real fields — MapData: timeStamp, msgIssueRevision, and 1..32 IntersectionGeometry, \
         each with id, revision, refPoint (Position3D lat/long/elevation), laneWidth and \
         1..255 GenericLane carrying laneID, ingressApproach, egressApproach, \
         laneAttributes (directionalUse, sharedWith, laneType: vehicle only), maneuvers, an \
         explicit NodeListXY node set of 2..63 NodeXY over all six node-XY alternatives and \
         node-LatLon, and 1..16 Connection (connectingLane with its maneuver, \
         remoteIntersection, signalGroup, userClass, connectionID)."
            .to_string(),
    );
    card.limitations.push(
        "Unimplemented and REFUSED rather than skipped — SPAT: name, regional, \
         IntersectionState.name/enabledLanes/maneuverAssistList/regional, \
         MovementState.movementName/maneuverAssistList/regional, \
         MovementEvent.speeds/regional. Refusing is not caution, it is correctness: PER \
         carries no tags, so a decoder that stepped over an unmodelled element would \
         misread every element after it."
            .to_string(),
    );
    card.limitations.push(
        "Unimplemented and REFUSED — MapData: layerType, layerID, roadSegments, \
         dataParameters, restrictionList, regional, IntersectionGeometry.name/speedLimits/\
         preemptPriorityData/regional, GenericLane.name/overlays/regional, \
         NodeListXY.computed (a ComputedLane), NodeXY.attributes, the seven \
         LaneTypeAttributes alternatives other than vehicle, and \
         NodeOffsetPointXY.regional."
            .to_string(),
    );
    card.limitations.push(
        "Because layerType and layerID are refused, a MAP from this codec is a conformant \
         MapData *encoding* but not a CTI 4501-conformant *message*: that profile expects \
         both fields. Nothing in the simulator reads them, and encoding an enumeration whose \
         width has not been verified would risk shifting every field after it."
            .to_string(),
    );
    card.limitations.push(
        "Every SEQUENCE and CHOICE extension marker in the MAP structure is a named \
         constant in v2xw_msg::j2735::map::assumptions, because the J2735 ASN.1 is \
         git-ignored (build decision D3) and is not in this checkout, so the markers could \
         not be re-read. A wrong marker is a one-bit shift of everything after it and is \
         invisible to a round-trip test, since encoder and decoder share the assumption."
            .to_string(),
    );
    card.limitations.push(
        "MovementPhaseState is encoded as a 4-bit non-extensible ENUMERATED index. The \
         size-model derivation in codec/size-model/j2735, written while the ASN.1 was \
         readable, counted 5 bits for the same field. One of the two is wrong; until an \
         oracle run says which, a SPaT from this codec is one bit per movement event \
         smaller than the size model reports."
            .to_string(),
    );

    card.assumptions = vec![
        "The message set is SAE J2735 2024-09. The constraint values, bit-string sizes and \
         CHOICE alternative counts were recalled rather than re-read: third_party/asn1/j2735/ \
         is git-ignored (build decision D3) and is absent from this checkout."
            .to_string(),
        "Latitude, Longitude, Elevation and MsgCount reuse the constants of \
         v2xw_msg::j2735::bsm, which the 235-vector pycrate oracle validated, so those four \
         field widths are as well evidenced here as they are in a BSM."
            .to_string(),
        "MinuteOfTheYear and DSecond are derived from the scenario wall clock and SimTime, \
         never from a system clock, and ignore leap seconds — the same assumption \
         v2xw_msg::j2735::bsm::sec_mark already makes."
            .to_string(),
        "A node offset keeps the node-XY alternative it was encoded in, so a decode \
         followed by an encode is byte-exact even when a narrower alternative would have \
         held the value."
            .to_string(),
    ];

    card.ignores = vec![
        "Regional extensions entirely, in every structure of both messages. They are the \
         reason J2735 cannot be code-generated at all (build decision D2)."
            .to_string(),
        "SRM, SSM and PSM, which remain in the validated size model \
         codec/size-model/j2735 with placeholder bytes."
            .to_string(),
        "The IEEE 1609.2 security envelope, which v2xw-sec puts around these payloads."
            .to_string(),
    ];

    // The unverified structural choices, as registry parameters, so they appear on the
    // todo-calibrate report (rule R1) instead of living only in prose. Each carries the
    // plan that would settle it.
    const ORACLE_PLAN: &str = "Restore third_party/asn1/j2735/ from a machine that holds an \
                               SAE licence, rebuild the pycrate oracle environment \
                               (tests/oracle/compile_j2735.py), and run \
                               `cargo test -p v2xw-msg --test j2735_infra_oracle`. The \
                               oracle compares bytes against an independent implementation \
                               of the real ASN.1, which is the only thing that can settle a \
                               structural question this codec answered from recall.";
    let unverified = [
        (
            "spat_movement_phase_state_index_bits",
            "bit",
            serde_json::json!(spat::MOVEMENT_PHASE_STATE_BITS),
            "MovementPhaseState is encoded as a non-extensible 10-value ENUMERATED (X.691 \
             clause 14.3); the size-model derivation counted 5 bits, which is what an \
             extensible one costs",
        ),
        (
            "map_position3d_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_POSITION_3D),
            "whether Position3D carries `...`, which adds one bit before its optional-field \
             bit-map",
        ),
        (
            "map_lane_attributes_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_LANE_ATTRIBUTES),
            "whether LaneAttributes carries `...`; modelled as not, by analogy with \
             IntersectionReferenceID, whose 1-bit preamble the size-model derivation records",
        ),
        (
            "map_node_xy_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_NODE_XY),
            "whether NodeXY carries `...`",
        ),
        (
            "map_node_list_xy_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_NODE_LIST_XY),
            "whether the NodeListXY CHOICE carries `...`, which adds one bit before its \
             alternative index",
        ),
        (
            "map_lane_type_attributes_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_LANE_TYPE_ATTRIBUTES),
            "whether the LaneTypeAttributes CHOICE carries `...`",
        ),
        (
            "map_connection_has_extension_marker",
            "-",
            serde_json::json!(map::assumptions::EXT_CONNECTION),
            "whether Connection carries `...`",
        ),
        (
            "map_lane_width_max_cm",
            "cm",
            serde_json::json!(map::LANE_WIDTH_MAX),
            "the upper bound of LaneWidth, which fixes the field's width at 15 bits; the \
             size-model derivation counted 16",
        ),
    ];
    for (name, unit, default, what) in unverified {
        let mut parameter = Parameter::new(name, unit, default, Source::todo_calibrate(what));
        parameter.calibration = Some(ORACLE_PLAN.to_string());
        card.parameters.push(parameter);
    }

    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "SAE J2735 2024-09 — SPAT and MapData (recalled; the modules are not in this \
             checkout and are never redistributed, per build decision D3)",
        ),
        Source::new(
            SourceKind::Standard,
            "SAE J2735 2024-09 — MessageFrame, DSRCmsgID mapData(18) and \
             signalPhaseAndTimingMessage(19)",
        ),
        Source::new(
            SourceKind::Standard,
            "ITU-T X.691 (02/2021) — Packed Encoding Rules, UNALIGNED variant: clauses 11.2, \
             11.5, 11.9, 14, 16, 19, 23",
        ),
        Source::new(
            SourceKind::Code,
            "this repository: crates/v2xw-msg/src/size_model.rs — the derivation table \
             written while the J2735 modules were on disk, which corroborates the SPAT \
             preamble widths and list determinants used here",
        ),
        Source::new(
            SourceKind::Code,
            "this repository: docs/design/12-build-decisions.md D2 (encoder strategy), D3 \
             (the J2735 modules are never committed), D9 (writer-side quantisation)",
        ),
    ];

    card.validation = Validation {
        // Deliberately `UnitTested`, a rung below the BSM codec's `LiteratureChecked`. The
        // encoder round-trips, refuses what it cannot model and pins two exact sizes, and
        // that is all: no published SPaT or MAP size exists to check against
        // (04-models.md §8.2 records "SPaT typical: none found") and no independent
        // implementation has seen these bytes.
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(
            SourceKind::Code,
            "NOT YET VALIDATED against an oracle. tests/j2735_infra_oracle.rs holds the \
             harness and the vectors; it skips with a message because neither the J2735 \
             ASN.1 nor the pycrate environment is on this machine. The BSM codec's card \
             records what a completed run looks like: 235 vectors, byte-identical.",
        )],
        tests: vec![
            "j2735::spat::tests::a_minimal_spat_is_eleven_octets".to_string(),
            "j2735::spat::tests::round_trip_is_exact".to_string(),
            "j2735::spat::tests::an_unmodelled_element_is_refused_rather_than_skipped"
                .to_string(),
            "j2735::map::tests::a_minimal_map_is_twenty_eight_octets".to_string(),
            "j2735::map::tests::round_trip_is_exact".to_string(),
            "j2735::map::tests::a_node_offset_keeps_its_alternative_so_the_bytes_are_stable"
                .to_string(),
            "j2735::map::tests::the_structural_assumptions_are_the_ones_the_module_documents"
                .to_string(),
            "j2735_infra_oracle::rust_spat_encodings_match_pycrate_byte_for_byte (skips)"
                .to_string(),
            "j2735_infra_oracle::rust_map_encodings_match_pycrate_byte_for_byte (skips)"
                .to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::SizeSource;
    use crate::j2735::spat::{
        IntersectionReferenceId, IntersectionState, IntersectionStatus, MovementEvent,
        MovementPhaseState, MovementState, Spat,
    };

    fn spat_bytes() -> Vec<u8> {
        spat::encode_spat(&Spat::one(IntersectionState {
            id: IntersectionReferenceId::new(1),
            revision: 0,
            status: IntersectionStatus::NONE,
            moy: None,
            time_stamp: None,
            states: vec![MovementState::current(
                1,
                MovementEvent::phase(MovementPhaseState::StopAndRemain),
            )],
        }))
        .expect("encodes")
        .bytes
    }

    fn map_bytes() -> Vec<u8> {
        let lane = map::GenericLane {
            lane_id: 1,
            ingress_approach: Some(1),
            egress_approach: None,
            attributes: map::LaneAttributes::vehicle(map::LaneDirection::INGRESS),
            maneuvers: Some(map::AllowedManeuvers::STRAIGHT),
            nodes: vec![
                map::NodeXy::offset(0, 0).expect("fits"),
                map::NodeXy::offset(150, 2_000).expect("fits"),
            ],
            connects_to: vec![map::Connection::signalised(2, 1)],
        };
        map::encode_map(&map::MapData {
            time_stamp: None,
            msg_issue_revision: 1,
            intersections: vec![map::IntersectionGeometry {
                id: IntersectionReferenceId::new(1),
                revision: 1,
                ref_point: map::Position3D {
                    lat: 407_440_000,
                    lon: -739_900_000,
                    elevation: Some(100),
                },
                lane_width_cm: Some(map::lane_width_cm(3.5)),
                lanes: vec![lane],
            }],
        })
        .expect("encodes")
        .bytes
    }

    #[test]
    fn the_card_validates_and_names_the_family() {
        let codec = J2735InfraCodec::new();
        codec.card().validate().expect("card validates");
        codec.card().check_api_version().expect("api version");
        assert_eq!(codec.family(), Family::Codec);
        assert_eq!(codec.id(), J2735_INFRA_CODEC_ID);
    }

    /// The card is where a run's manifest learns that these bytes are unvalidated. A
    /// silent deletion of that sentence is a defect in its own right — it is the one thing
    /// that must never happen to this crate's claims.
    #[test]
    fn the_card_says_the_bytes_are_not_yet_oracle_validated() {
        let card = J2735InfraCodec::new().card().clone();
        assert_eq!(card.validation.status, ValidationStatus::UnitTested);
        let references = card
            .validation
            .references
            .iter()
            .map(|s| s.reference.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(references.contains("NOT YET VALIDATED"), "{references}");

        let text = card.limitations.join("\n");
        for needle in [
            "real UPER",
            "not",
            "assumptions",
            "MovementPhaseState",
            "REFUSED",
        ] {
            assert!(text.contains(needle), "the card should mention {needle}");
        }
        assert!(
            card.purpose.contains("NOT yet cross-validated"),
            "even the one-line purpose must carry the caveat: {}",
            card.purpose
        );
        // And the unverified structural choices must be on the todo-calibrate report.
        let todo: Vec<&str> = card.todo_calibrate().map(|p| p.name.as_str()).collect();
        assert!(todo.iter().any(|n| n.contains("extension_marker")), "{todo:?}");
        assert!(
            todo.iter()
                .any(|n| *n == "spat_movement_phase_state_index_bits"),
            "{todo:?}"
        );
    }

    #[test]
    fn it_claims_exactly_spat_and_map() {
        let codec = J2735InfraCodec::new();
        assert!(codec.supports(MsgType::Spat));
        assert!(codec.supports(MsgType::Map));
        for ty in MsgType::ALL {
            if ty != MsgType::Spat && ty != MsgType::Map {
                assert!(!codec.supports(ty), "{ty} should not be claimed");
            }
        }
    }

    #[test]
    fn the_seam_validates_bytes_and_returns_them_unchanged() {
        let codec = J2735InfraCodec::new();
        for (ty, bytes) in [(MsgType::Spat, spat_bytes()), (MsgType::Map, map_bytes())] {
            let encoded = codec
                .encode(&Message::HandEncoded {
                    ty,
                    bytes: bytes.clone(),
                })
                .unwrap_or_else(|e| panic!("{ty} should be valid: {e}"));
            assert_eq!(encoded.bytes, bytes, "{ty}");
            assert_eq!(encoded.size_source, SizeSource::Uper, "{ty}");
            assert!(encoded.is_real(), "{ty}");

            let Message::HandEncoded { ty: back_ty, bytes: back } =
                codec.decode(&bytes, ty).expect("decodes")
            else {
                unreachable!("this codec returns HandEncoded")
            };
            assert_eq!(back_ty, ty);
            assert_eq!(back, bytes);
        }
    }

    #[test]
    fn the_seam_refuses_bytes_that_are_not_the_message_it_was_told() {
        let codec = J2735InfraCodec::new();
        // A MAP's bytes are not a SPaT, and the seam must not pretend otherwise.
        assert!(codec.decode(&map_bytes(), MsgType::Spat).is_err());
        assert!(codec.decode(&spat_bytes(), MsgType::Map).is_err());
        assert!(codec.decode(&[0xa5; 16], MsgType::Spat).is_err());
        let err = codec
            .encode(&Message::HandEncoded {
                ty: MsgType::Bsm,
                bytes: spat_bytes(),
            })
            .expect_err("not ours");
        assert!(matches!(err, CodecError::Unsupported { .. }), "{err}");
    }

    #[test]
    fn a_codec_is_usable_as_a_trait_object() {
        let codec: Box<dyn MessageCodec> = Box::new(J2735InfraCodec::new());
        assert_eq!(codec.message_types(), &[MsgType::Spat, MsgType::Map]);
    }
}
