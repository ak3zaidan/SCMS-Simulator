//! The `MessageCodec` seam — 03-interfaces.md §6.
//!
//! One trait, three tiers of implementation behind it (build decision D2):
//!
//! | Tier | Implementation | `size_source` | Bytes are |
//! |---|---|---|---|
//! | real ASN.1 | [`EtsiUperCodec`] over `rasn`-generated ETSI bindings (CAM, DENM) | [`SizeSource::Uper`] | the wire bytes |
//! | real ASN.1 | [`crate::sec_types::coer`] over the 1609.2 bindings | [`SizeSource::Coer`] | the wire bytes |
//! | hand-written | [`crate::j2735::J2735BsmCodec`] | [`SizeSource::Uper`] | the wire bytes of the subset we fill, oracle-validated |
//! | hand-written | [`crate::j2735::J2735InfraCodec`] (SPaT, MAP) | [`SizeSource::Uper`] | the wire bytes of the subset we fill, **not yet oracle-validated** |
//! | size model | [`crate::size_model::J2735SizeCodec`] (PSM, SRM, SSM) | [`SizeSource::SizeModel`] | **placeholders** of exact modelled length |
//! | size model | [`crate::etsi_size::EtsiSizeCodec`] (CPM, VAM) | [`SizeSource::SizeModel`] | **placeholders** of exact modelled length |
//!
//! [`Encoded::size_source`] is what tells the tiers apart at runtime, and nothing else in
//! the engine may assume the bytes mean anything: a metric that counted ones in a
//! size-model payload would be measuring a fill pattern.
//!
//! `size_source` answers "are these wire bytes?" and nothing more. The finer question —
//! whether anyone has *checked* those bytes against the standard — is
//! [`crate::evidence`], and a report that says "byte-exact" must consult that instead: the
//! two hand-written tiers above share one `SizeSource` and carry very different evidence.

use v2xw_core::model::Model;

use crate::error::CodecError;

/// The message types the engine knows about (03-interfaces.md §6).
///
/// Closed on purpose: every variant needs a codec entry, a size-model row or a generator,
/// and adding one silently would leave a type that no codec claims and no test covers.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MsgType {
    /// SAE J2735 Basic Safety Message.
    Bsm,
    /// ETSI Cooperative Awareness Message (EN 302 637-2 / TS 103 900).
    Cam,
    /// ETSI Decentralized Environmental Notification Message (EN 302 637-3 / TS 103 831).
    Denm,
    /// Signal Phase and Timing.
    Spat,
    /// Intersection geometry (MAP / MAPEM).
    Map,
    /// SAE J2945/9 Personal Safety Message.
    Psm,
    /// ETSI VRU Awareness Message (TS 103 300-3).
    Vam,
    /// ETSI Collective Perception Message (TS 103 324).
    Cpm,
    /// Signal Request Message.
    Srm,
    /// Signal Status Message.
    Ssm,
    /// IEEE 1609.3 WAVE Service Advertisement.
    Wsa,
    /// Certificate Revocation List.
    Crl,
    /// Misbehaviour report (TS 103 759).
    Mbr,
}

impl MsgType {
    /// Every variant, in declaration order. Handy for table-driven tests.
    pub const ALL: [MsgType; 13] = [
        MsgType::Bsm,
        MsgType::Cam,
        MsgType::Denm,
        MsgType::Spat,
        MsgType::Map,
        MsgType::Psm,
        MsgType::Vam,
        MsgType::Cpm,
        MsgType::Srm,
        MsgType::Ssm,
        MsgType::Wsa,
        MsgType::Crl,
        MsgType::Mbr,
    ];

    /// The lower-case spelling used in records, cards and scenario files.
    pub const fn as_str(self) -> &'static str {
        match self {
            MsgType::Bsm => "bsm",
            MsgType::Cam => "cam",
            MsgType::Denm => "denm",
            MsgType::Spat => "spat",
            MsgType::Map => "map",
            MsgType::Psm => "psm",
            MsgType::Vam => "vam",
            MsgType::Cpm => "cpm",
            MsgType::Srm => "srm",
            MsgType::Ssm => "ssm",
            MsgType::Wsa => "wsa",
            MsgType::Crl => "crl",
            MsgType::Mbr => "mbr",
        }
    }

    /// The ETSI `MessageId` this type is carried under in an `ItsPduHeader`, if it has one.
    ///
    /// Values from `ETSI-ITS-CDD.asn`, `MessageId ::= INTEGER { denm(1), cam(2), … }`.
    /// The SAE messages (BSM, PSM) have no ETSI message id: they are identified by the
    /// J2735 `MessageFrame` `messageId` instead, which is a different number for the same
    /// message — a SPaT is `spat(4)` here and `signalPhaseAndTimingMessage(19)` there. The
    /// hand-written codecs encode that wrapper themselves
    /// ([`crate::j2735::bsm::BSM_MESSAGE_ID`], [`crate::j2735::spat::SPAT_MESSAGE_ID`],
    /// [`crate::j2735::map::MAP_MESSAGE_ID`]); this accessor is only ever about the ETSI
    /// `ItsPduHeader`.
    pub const fn its_message_id(self) -> Option<u8> {
        match self {
            MsgType::Denm => Some(1),
            MsgType::Cam => Some(2),
            MsgType::Spat => Some(4),
            MsgType::Map => Some(5),
            MsgType::Srm => Some(9),
            MsgType::Ssm => Some(10),
            MsgType::Cpm => Some(14),
            MsgType::Vam => Some(16),
            MsgType::Bsm | MsgType::Psm | MsgType::Wsa | MsgType::Crl | MsgType::Mbr => None,
        }
    }
}

impl core::fmt::Display for MsgType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A size model's version, as it appears in [`SizeSource::SizeModel`] and in the manifest.
///
/// A plain triple rather than a string so it is `Copy`, orderable and cheap to put in
/// every [`Encoded`]. It is bumped whenever a table value or a tolerance changes, because
/// a recorded run's sizes are only interpretable against the version that produced them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SizeModelVersion {
    /// Bumped when a message type is added or removed.
    pub major: u16,
    /// Bumped when a modelled value or a tolerance changes.
    pub minor: u16,
    /// Bumped for anything that cannot change a number (comments, sources).
    pub patch: u16,
}

impl SizeModelVersion {
    /// A version literal.
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl core::fmt::Display for SizeModelVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Where an [`Encoded`]'s size came from — and therefore what its bytes mean.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "version")]
pub enum SizeSource {
    /// ASN.1 Unaligned Packed Encoding Rules. The bytes are the wire bytes and the size is
    /// exact (invariant I-S2). Used by the ETSI facilities layer and by the hand-written
    /// J2735 BSM codec.
    Uper,
    /// ASN.1 Canonical Octet Encoding Rules, the encoding IEEE 1609.2 and ETSI TS 103 097
    /// mandate for the security envelope and certificates [TS 103 097 V2.1.1 §4.1].
    Coer,
    /// The validated size model of 04-models.md §8.4. The size is exact to the model; the
    /// **bytes are placeholders** and carry no fields.
    SizeModel(SizeModelVersion),
}

impl SizeSource {
    /// True when the bytes are real wire bytes that a decoder can read back.
    pub const fn is_real(self) -> bool {
        matches!(self, SizeSource::Uper | SizeSource::Coer)
    }
}

/// An encoded message: its bytes, its length, and where that length came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The encoded octets, or placeholders of the modelled length.
    pub bytes: Vec<u8>,
    /// `bytes.len()`, carried separately because 03-interfaces.md §6 declares it and
    /// because the recorder writes the size without the payload.
    pub size: u32,
    /// Which tier produced this.
    pub size_source: SizeSource,
}

impl Encoded {
    /// Wraps real UPER bytes.
    pub fn uper(bytes: Vec<u8>) -> Self {
        Self::real(bytes, SizeSource::Uper)
    }

    /// Wraps real COER bytes.
    pub fn coer(bytes: Vec<u8>) -> Self {
        Self::real(bytes, SizeSource::Coer)
    }

    fn real(bytes: Vec<u8>, size_source: SizeSource) -> Self {
        // `as` would silently wrap a >4 GiB payload into a small number. Nothing here can
        // produce one (the largest V2X PDU modelled is a few kilobytes), but `size` is what
        // every downstream byte count is derived from, so it saturates rather than wraps.
        let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        Self {
            bytes,
            size,
            size_source,
        }
    }

    /// A size-model placeholder of exactly `size` bytes.
    ///
    /// The fill is `0xA5`, not zero: a zero-filled buffer is indistinguishable from an
    /// uninitialised one, and a test that accidentally decodes a placeholder should fail
    /// loudly rather than read a plausible all-zero message.
    pub fn size_model(size: u32, version: SizeModelVersion) -> Self {
        Self {
            bytes: vec![PLACEHOLDER_FILL; size as usize],
            size,
            size_source: SizeSource::SizeModel(version),
        }
    }

    /// True when [`Encoded::bytes`] are real wire bytes.
    pub const fn is_real(&self) -> bool {
        self.size_source.is_real()
    }
}

/// The byte a size-model placeholder is filled with.
pub const PLACEHOLDER_FILL: u8 = 0xA5;

/// A message in the form a codec encodes.
///
/// `#[non_exhaustive]`: this crate grows a variant per real codec, and the hand-written
/// J2735 BSM codec adds its own. Anything already in bytes — a hand-encoded payload, a
/// replayed capture — travels as [`Message::HandEncoded`] and needs no new variant.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Message {
    /// An ETSI CAM, as the generated bindings model it.
    Cam(Box<crate::asn1::facilities::cam_pdu_descriptions::CAM>),
    /// An ETSI DENM, as the generated bindings model it.
    Denm(Box<crate::asn1::facilities::denm_pdu_description::DENM>),
    /// A payload some other real encoder already produced: the J2735 BSM codec's output,
    /// or bytes read back from a recording. `ty` says what the bytes are.
    HandEncoded {
        /// What the bytes encode.
        ty: MsgType,
        /// The wire bytes.
        bytes: Vec<u8>,
    },
    /// A message the engine only *sizes*: everything in the size-model tier.
    Modeled(crate::size_model::SizeRequest),
}

impl Message {
    /// Which message type this is.
    pub fn msg_type(&self) -> MsgType {
        match self {
            Message::Cam(_) => MsgType::Cam,
            Message::Denm(_) => MsgType::Denm,
            Message::HandEncoded { ty, .. } => *ty,
            Message::Modeled(request) => request.ty,
        }
    }
}

/// Encode a message to bytes, and read them back (03-interfaces.md §6).
///
/// Extends [`Model`], so every codec carries a model card naming the standard it
/// implements and the tier it belongs to — which is how a run's manifest can say whether
/// its CAM sizes were measured or modelled.
pub trait MessageCodec: Model {
    /// The message types this codec implements.
    fn message_types(&self) -> &[MsgType];

    /// Encodes to real bytes (ASN.1 UPER/COER) or, in the size-model tier, to a
    /// placeholder of exact modelled size. [`Encoded::size_source`] says which.
    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError>;

    /// Decodes bytes previously produced by this codec.
    ///
    /// The size-model tier returns [`CodecError::PlaceholderBytes`]: there is nothing to
    /// decode, and returning a default-filled message instead would let a caller believe a
    /// modelled payload had contents.
    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError>;

    /// Whether this codec claims `t`. Defaulted from [`MessageCodec::message_types`].
    fn supports(&self, t: MsgType) -> bool {
        self.message_types().contains(&t)
    }
}

/// Model id of the ETSI real-UPER codec.
pub const ETSI_UPER_CODEC_ID: &str = "codec/uper/rasn-etsi";

/// The real ASN.1 UPER codec for the ETSI facilities layer.
///
/// Encodes and decodes CAM and DENM through the bindings `build.rs` generates from the
/// ETSI forge modules. `04-models.md` §8.3 names it `codec/uper/rasn-etsi`.
///
/// Stateless, so one instance serves every node; [`EtsiUperCodec::new`] is the only
/// constructor and building the card is its only cost.
#[derive(Debug, Clone)]
pub struct EtsiUperCodec {
    card: v2xw_core::card::ModelCard,
}

impl Default for EtsiUperCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl EtsiUperCodec {
    /// The types this codec implements today.
    ///
    /// CPM and VAM are generatable from the same forge modules (the asset survey compiled
    /// both) and are still not here, for one reason: **their modules are not in this
    /// checkout**. `third_party/asn1/etsi/` holds the CDD, CAM, DENM, 1609.2 and
    /// TS 103 097 modules and nothing else, and adding a missing file to the `FACILITIES`
    /// unit would fail the build rather than generate anything. They arrive by committing
    /// the two modules, adding them to that unit, and adding a variant to [`Message`];
    /// until then [`crate::etsi_size`] sizes them and says so. Its module documentation
    /// lists the steps.
    pub const TYPES: [MsgType; 2] = [MsgType::Cam, MsgType::Denm];

    /// Builds the codec and its card.
    pub fn new() -> Self {
        Self { card: card() }
    }
}

fn card() -> v2xw_core::card::ModelCard {
    use v2xw_core::card::{
        Family, ModelCard, Source, SourceKind, Tier, Validation, ValidationStatus,
    };

    let mut card = ModelCard::new(
        ETSI_UPER_CODEC_ID,
        Family::Codec,
        "1.0.0",
        "Real ASN.1 UPER encoding of the ETSI facilities layer (CAM, DENM) through rasn \
         bindings generated from the ETSI forge modules.",
    );
    // Both tiers: the bytes are the same at every fidelity. A codec has no cheap variant —
    // the abstract tier still needs an exact size, and this one produces it for free.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.assumptions = vec![
        "CAM is TS 103 900 (Release 2) and DENM is TS 103 831 (Release 2), both over \
         ETSI-ITS-CDD Release 2. The Release 1 spellings (EN 302 637-2/-3 over ITS-Container) \
         are a different data dictionary and are not interchangeable with these."
            .to_string(),
        "generationDeltaTime is TimestampIts mod 65 536 ms, and TimestampIts is derived from \
         the scenario wall clock's 1609.2 epoch, so it ignores leap seconds between 2004 and \
         the scenario date."
            .to_string(),
    ];
    // The byte-exactness statement comes from crate::evidence, the one table that says
    // which payloads are real bytes and which are not, so no two cards can describe the
    // same message differently.
    card.limitations = crate::evidence::card_statement(ETSI_UPER_CODEC_ID);
    card.limitations.extend([
        "CAM special-vehicle containers and DENM a-la-carte containers are not filled by this \
         crate's builders; they encode correctly if a caller constructs them."
            .to_string(),
        "No ETSI conformance test vectors are checked yet: the evidence here is round-trip \
         equality plus field-by-field range checks, not third-party bytes. That is why \
         crate::evidence calls CAM and DENM `generated-from-module` rather than \
         oracle-validated: a defect in rasn itself would not show."
            .to_string(),
        "CPM and VAM are generatable from the same forge modules and are NOT encoded here, \
         because CPM-PDU-Descriptions.asn and VAM-PDU-Descriptions.asn are absent from \
         third_party/asn1/etsi in this checkout. They are sized by codec/size-model/etsi \
         instead, whose documentation lists the four steps that make them real."
            .to_string(),
    ]);
    card.ignores = vec![
        "SAE J2735 entirely. rasn-compiler cannot compile its RegionalExtension idiom \
         (build decision D2), so BSM, SPaT and MAP are hand-written (crate::j2735) and \
         PSM/SRM/SSM are modelled."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 900 V2.1.1 (CAM, Release 2) — CAM-PDU-Descriptions.asn, forge commit 649ada78da45",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 831 V2.2.1 (DENM, Release 2) — DENM-PDU-Descriptions.asn, forge commit 58472e2644a6",
        ),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 102 894-2 (Common Data Dictionary, Release 2) — ETSI-ITS-CDD.asn, forge commit 615593c445d0",
        ),
        Source::new(
            SourceKind::Standard,
            "ITU-T X.691 (ASN.1 Packed Encoding Rules), unaligned variant",
        ),
        Source::new(
            SourceKind::Code,
            "rasn 0.28 runtime; rasn-compiler 0.16 codegen",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "C2C-CC TR 2052 Tables 6-1, 6-2 — field CAM sizes (min 182-199 B with digest, \
             overall mean 357 B with the field certificate mix); 04-models.md §8.2",
        )],
        tests: vec![
            "cam::tests::round_trip_is_exact".to_string(),
            "cam::tests::typical_cam_size_is_consistent_with_the_field_range".to_string(),
            "denm::tests::round_trip_is_exact".to_string(),
            "codec_sizes_are_stable_across_runs".to_string(),
        ],
    };
    card
}

impl Model for EtsiUperCodec {
    fn card(&self) -> &v2xw_core::card::ModelCard {
        &self.card
    }
}

impl MessageCodec for EtsiUperCodec {
    fn message_types(&self) -> &[MsgType] {
        &Self::TYPES
    }

    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError> {
        match msg {
            Message::Cam(cam) => Ok(Encoded::uper(uper_encode(MsgType::Cam, cam.as_ref())?)),
            Message::Denm(denm) => Ok(Encoded::uper(uper_encode(MsgType::Denm, denm.as_ref())?)),
            other => Err(CodecError::Unsupported {
                codec: ETSI_UPER_CODEC_ID.to_string(),
                ty: other.msg_type(),
            }),
        }
    }

    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError> {
        match t {
            MsgType::Cam => Ok(Message::Cam(Box::new(uper_decode(t, bytes)?))),
            MsgType::Denm => Ok(Message::Denm(Box::new(uper_decode(t, bytes)?))),
            other => Err(CodecError::Unsupported {
                codec: ETSI_UPER_CODEC_ID.to_string(),
                ty: other,
            }),
        }
    }
}

/// UPER-encodes any generated type, turning `rasn`'s error into ours.
pub(crate) fn uper_encode<T: rasn::Encode>(ty: MsgType, value: &T) -> Result<Vec<u8>, CodecError> {
    rasn::uper::encode(value).map_err(|e| CodecError::Encode {
        ty,
        detail: e.to_string(),
    })
}

/// UPER-decodes any generated type, turning `rasn`'s error into ours.
pub(crate) fn uper_decode<T: rasn::Decode>(ty: MsgType, bytes: &[u8]) -> Result<T, CodecError> {
    rasn::uper::decode(bytes).map_err(|e| CodecError::Decode {
        ty,
        len: bytes.len(),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_validates_and_names_the_family() {
        let codec = EtsiUperCodec::new();
        codec.card().validate().expect("card validates");
        assert_eq!(codec.family(), v2xw_core::card::Family::Codec);
        assert_eq!(codec.id(), ETSI_UPER_CODEC_ID);
    }

    #[test]
    fn it_claims_exactly_cam_and_denm() {
        let codec = EtsiUperCodec::new();
        assert!(codec.supports(MsgType::Cam));
        assert!(codec.supports(MsgType::Denm));
        for ty in MsgType::ALL {
            if ty != MsgType::Cam && ty != MsgType::Denm {
                assert!(!codec.supports(ty), "{ty} should not be claimed");
            }
        }
    }

    #[test]
    fn an_unsupported_type_is_refused_by_name() {
        let codec = EtsiUperCodec::new();
        let err = codec.decode(&[0u8; 4], MsgType::Bsm).unwrap_err();
        assert!(
            err.to_string().contains("bsm"),
            "the error should name the type: {err}"
        );
    }

    #[test]
    fn a_codec_is_usable_as_a_trait_object() {
        // How the engine holds it (ADR 0007 §8). If `MessageCodec` stopped being
        // dyn-compatible this would not compile.
        let codec: Box<dyn MessageCodec> = Box::new(EtsiUperCodec::new());
        assert_eq!(codec.message_types().len(), 2);
    }

    #[test]
    fn placeholder_bytes_are_marked_and_not_real() {
        let version = SizeModelVersion::new(1, 0, 0);
        let e = Encoded::size_model(39, version);
        assert_eq!(e.size, 39);
        assert_eq!(e.bytes.len(), 39);
        assert!(!e.is_real());
        assert!(e.bytes.iter().all(|&b| b == PLACEHOLDER_FILL));
        assert_eq!(e.size_source, SizeSource::SizeModel(version));
    }

    #[test]
    fn message_ids_match_the_cdd() {
        assert_eq!(MsgType::Denm.its_message_id(), Some(1));
        assert_eq!(MsgType::Cam.its_message_id(), Some(2));
        assert_eq!(MsgType::Bsm.its_message_id(), None);
    }

    #[test]
    fn size_source_round_trips_through_serde() {
        for s in [
            SizeSource::Uper,
            SizeSource::Coer,
            SizeSource::SizeModel(SizeModelVersion::new(1, 2, 3)),
        ] {
            let json = serde_json::to_string(&s).expect("serialises");
            let back: SizeSource = serde_json::from_str(&json).expect("deserialises");
            assert_eq!(s, back, "{json}");
        }
    }
}
