//! The validated size model for the J2735 messages this crate does not really encode —
//! 04-models.md §8.4, build decision D2.
//!
//! # Why this exists at all
//!
//! `rasn-compiler` cannot compile SAE J2735: its `RegionalExtension {REG-EXT-ID-AND-TYPE :
//! Set}` idiom is a parameterised type over an information object class, which produces 170
//! compile errors, and deleting the optional regional fields still leaves 105 (D2, and the
//! asset survey's section E.2). So the US infrastructure and VRU messages get a *size*
//! model: [`Encoded::size`] is exact to the model and the bytes are placeholders. A
//! consumer tells the two apart with [`crate::codec::SizeSource`], which this codec always
//! sets to [`crate::codec::SizeSource::SizeModel`].
//!
//! # What a row says
//!
//! Each [`SizeEntry`] is a `(message type, content profile)` pair with a fixed base, a
//! per-element increment, and **its anchors**: the published numbers it is checked against
//! (invariant I-S2). [`SizeEntry::check`] is the check, and
//! [`tests::every_entry_satisfies_its_own_anchors`] runs it over the whole table.
//!
//! Anchors are recorded at the layer scope their source states and converted to payload
//! scope before comparison, because comparing a modelled UPER payload against a figure that
//! included a 1609.2 envelope and a MAC header is how size models come to be quietly wrong.
//! The deductions are [`ENVELOPE_DIGEST_B`], [`ENVELOPE_CERT_B`] and [`WSMP_HEADER_B`],
//! each from 04-models.md §9.1 and §9.3.
//!
//! # The honest state of the evidence
//!
//! 04-models.md §8.2 records "SPaT typical: none found", "PSM: none". That is not a gap this
//! module can close by inventing a number, so every base value here is **derived from the
//! J2735 2024-09 ASN.1 structure** — counting UPER preamble bits, optional-field bits,
//! length determinants and constrained-integer widths for a stated content profile — and is
//! marked [`EntryStatus::DerivedNoAnchor`] with the calibration plan §8.4 prescribes:
//! *encode with the real module once imported, or with asn1c on a machine that holds an SAE
//! licence, and record the measured bytes*. The derivations are written out in the table
//! below so a reviewer can check the arithmetic without the ASN.1 in front of them.
//!
//! | Message | Profile | Base | Where the base comes from |
//! |---|---|---:|---|
//! | SPaT | minimal | 17 B | MessageFrame 4 B + SPAT preamble (1 extension + 3 optional bits) + `IntersectionStateList` length 5 b + one `IntersectionState`: 1+6 preamble, `id` 1+16, `revision` 7, `status` 16, `moy` 20, `timeStamp` 16, `MovementList` length 8 → 100 b ≈ 13 B |
//! | SPaT | typical | 17 B | same fixed part; the difference is in the increment |
//! | MAP | typical | 40 B | MessageFrame 4 B + `MapData` preamble + `IntersectionGeometry` id and `refPoint` (lat 32 b + long 32 b + elevation 16 b) + `laneWidth` 16 b + list determinants |
//! | PSM | typical | 27 B | MessageFrame 4 B + `PSMcoreData`: `basicType` 4 b, `secMark` 16 b, `msgCnt` 7 b, `id` 32 b, `position` 32+32+16 b, `accuracy` 12 b, `speed` 13 b, `heading` 16 b → ≈ 23 B |
//! | SRM | typical | 20 B | MessageFrame 4 B + `SignalRequestMessage` timestamp/second/sequence + `RequestorDescription` id and type |
//! | SSM | typical | 16 B | MessageFrame 4 B + `SignalStatusMessage` timestamp/second/sequence + `SignalStatusList` determinant |
//!
//! # Two of these rows are retired
//!
//! SPaT and MAP are now really encoded, by hand, in [`crate::j2735::spat`] and
//! [`crate::j2735::map`], so [`J2735SizeCodec`] no longer claims either type — a type
//! claimed by two codecs would make a run's sizes depend on resolution order. Their rows
//! stay in [`TABLE`] with [`SizeEntry::superseded_by`] set: a retired model value is the
//! only record of what runs recorded before the encoder existed were measuring, and it is
//! a cross-check on the encoder itself. [`ContentProfile`], [`lookup`] and [`SizeEntry`]
//! are also what [`crate::etsi_size`] builds the CPM and VAM rows from, so the two tables
//! share one definition of a row and one anchor check.
//!
//! | Message | Increment | Element | Where it comes from |
//! |---|---:|---|---|
//! | SPaT minimal | 4 B | movement state | `MovementState` 1+3+8+4 b + `MovementEvent` 1+3+5 b = 25 b |
//! | SPaT typical | 10 B | movement state | as above + `TimeChangeDetails` 5 optional bits + three `TimeMark` at 16 b = 78 b |
//! | MAP typical | 40 B | lane | `GenericLane` id and attributes plus about six `NodeXY` offsets and a connection list |
//! | PSM typical | 8 B | path-history point | the one *cited* increment in the table: C2C-CC TR 2052 §3.1 measures CAM path history at 8-9 B per entry, and a J2735 `PathHistoryPoint` carries the same three offsets and a time |
//! | SRM typical | 10 B | request | `SignalRequest` id, request type, inbound and outbound lane |
//! | SSM typical | 12 B | status | `SignalStatus` id, sequence, and one `SignalStatusPackage` |

use crate::codec::{Encoded, Message, MessageCodec, MsgType, SizeModelVersion};
use crate::error::{CodecError, MsgError};
use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;

/// Model id of the size-model codec, as 04-models.md §8.3 names it.
pub const J2735_SIZE_MODEL_ID: &str = "codec/size-model/j2735";

/// The table's version. Bumped whenever a number below changes.
pub const VERSION: SizeModelVersion = SizeModelVersion::new(1, 0, 0);

/// IEEE 1609.2 SPDU overhead with a digest signer, bytes (04-models.md §9.1: ≈ 93-94).
pub const ENVELOPE_DIGEST_B: u32 = 93;
/// IEEE 1609.2 SPDU overhead with an implicit certificate, bytes
/// (04-models.md §9.1 "≈ 87 + cert" and §9.2 "implicit ≈ 80").
pub const ENVELOPE_CERT_B: u32 = 87 + 80;
/// WSMP header for a J2735 message over 1609.3, bytes (04-models.md §9.3).
pub const WSMP_HEADER_B: u32 = 5;
/// `MessageFrame` wrapper: `messageId` (0..32767) plus the open-type length determinant.
pub const MESSAGE_FRAME_B: u32 = 4;

/// How much optional content a message carries.
///
/// Deliberately coarse. A size model that tried to enumerate every optional field would be
/// a codec, badly; three profiles is what a scenario can meaningfully choose between and
/// what the anchors can distinguish.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ContentProfile {
    /// Mandatory fields only.
    Minimal,
    /// What a deployment actually broadcasts: the mandatory fields plus the optional ones
    /// the relevant profile (CTI 4501, TS 103 301) requires.
    Typical,
    /// Every optional container a simulated sender might fill.
    Rich,
}

impl ContentProfile {
    /// The lower-case spelling used in scenarios and records.
    pub const fn as_str(self) -> &'static str {
        match self {
            ContentProfile::Minimal => "minimal",
            ContentProfile::Typical => "typical",
            ContentProfile::Rich => "rich",
        }
    }
}

impl core::fmt::Display for ContentProfile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the caller wants sized: a message type, a content profile, and how many of the
/// message's variable elements it carries.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SizeRequest {
    /// Which message.
    pub ty: MsgType,
    /// How much optional content.
    pub profile: ContentProfile,
    /// How many variable elements — movement states for a SPaT, lanes for a MAP, path
    /// points for a PSM, requests for an SRM, statuses for an SSM.
    pub elements: u32,
}

impl SizeRequest {
    /// A request for `elements` of the message's variable element at the typical profile.
    pub const fn typical(ty: MsgType, elements: u32) -> Self {
        Self {
            ty,
            profile: ContentProfile::Typical,
            elements,
        }
    }
}

/// The layer an anchor's number was measured or stated at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorScope {
    /// The ASN.1 payload alone — directly comparable to a modelled value.
    Payload,
    /// Payload plus a 1609.2 envelope with a digest signer.
    WithDigestEnvelope,
    /// Payload plus a 1609.2 envelope carrying a certificate.
    WithCertificateEnvelope,
    /// Payload plus a certificate envelope plus the WSMP header.
    WithCertificateEnvelopeAndHeaders,
}

impl AnchorScope {
    /// Bytes to subtract to bring a figure at this scope down to payload scope.
    pub const fn overhead_b(self) -> u32 {
        match self {
            AnchorScope::Payload => 0,
            AnchorScope::WithDigestEnvelope => ENVELOPE_DIGEST_B,
            AnchorScope::WithCertificateEnvelope => ENVELOPE_CERT_B,
            AnchorScope::WithCertificateEnvelopeAndHeaders => ENVELOPE_CERT_B + WSMP_HEADER_B,
        }
    }
}

/// What kind of constraint an anchor places on the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorKind {
    /// A reported typical or measured value; the model must sit inside the spread of all
    /// point anchors for the entry.
    Point,
    /// A stated ceiling; the model must not exceed it.
    UpperBound,
    /// A stated floor; the model must not fall below it.
    LowerBound,
}

/// One published number an entry is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    /// The number, as published.
    pub bytes: u32,
    /// What it constrains.
    pub kind: AnchorKind,
    /// Which layers it includes.
    pub scope: AnchorScope,
    /// Where it was published.
    pub source: &'static str,
}

impl Anchor {
    /// The anchor reduced to payload scope, saturating at zero.
    pub const fn payload_bytes(&self) -> u32 {
        self.bytes.saturating_sub(self.scope.overhead_b())
    }
}

/// How well evidenced an entry is (04-models.md §8.4's status vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryStatus {
    /// Two or more anchors; the tolerance is their spread.
    LiteratureChecked,
    /// Exactly one anchor; the tolerance is zero and the value is pinned to it.
    SingleAnchor,
    /// No anchor at all. The value is derived from the ASN.1 structure and carries a
    /// calibration plan.
    DerivedNoAnchor,
}

/// One row of the size model.
#[derive(Debug, Clone, Copy)]
pub struct SizeEntry {
    /// Which message.
    pub ty: MsgType,
    /// Which content profile.
    pub profile: ContentProfile,
    /// Bytes that are there regardless of how many elements the message carries.
    pub base_b: u32,
    /// Bytes added per variable element.
    pub per_element_b: u32,
    /// What the variable element is, for diagnostics and the card.
    pub element: &'static str,
    /// How many elements a message of this profile typically carries — the count the
    /// anchors are checked at.
    pub nominal_elements: u32,
    /// The published numbers this row is checked against.
    pub anchors: &'static [Anchor],
    /// How well evidenced it is.
    pub status: EntryStatus,
    /// The model id of a **real encoder** that has since replaced this row, if one has.
    ///
    /// A retired row is kept rather than deleted, for two reasons. It is the only record
    /// of what the modelled size *was*, so a run recorded before the encoder existed stays
    /// interpretable; and it is a cross-check on the encoder, because a hand-written
    /// encoder that disagrees with a structurally derived size model by more than a few
    /// bytes has a bug in one of them —
    /// [`tests::the_real_spat_encoder_lands_near_the_row_it_retired`] is that check.
    ///
    /// [`J2735SizeCodec`] no longer *claims* a type whose rows are all superseded, so
    /// [`assert_no_overlapping_claims`] still passes with the real codec registered. The
    /// rows remain sizable through [`lookup`] on purpose.
    pub superseded_by: Option<&'static str>,
}

impl SizeEntry {
    /// Modelled payload size for `elements` elements.
    pub const fn bytes(&self, elements: u32) -> u32 {
        self.base_b
            .saturating_add(self.per_element_b.saturating_mul(elements))
    }

    /// The entry's key, `"<message>/<profile>"`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.ty, self.profile)
    }

    /// The spread of the entry's *point* anchors in payload bytes, if it has any.
    ///
    /// This is the tolerance 04-models.md §8.4 says the card records: "a model value must
    /// fall inside the spread of the cited anchors for that message, and the tolerance
    /// recorded in the card is that spread".
    pub fn anchor_spread(&self) -> Option<(u32, u32)> {
        let mut lo = u32::MAX;
        let mut hi = 0u32;
        let mut any = false;
        for a in self.anchors {
            if a.kind == AnchorKind::Point {
                any = true;
                let v = a.payload_bytes();
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        any.then_some((lo, hi))
    }

    /// Checks the row against its own anchors at its nominal element count (I-S2).
    ///
    /// A row with no anchors passes, because there is nothing to check it against — but
    /// [`SizeEntry::status`] must then say [`EntryStatus::DerivedNoAnchor`], which
    /// [`tests::an_unanchored_entry_admits_it`] enforces.
    pub fn check(&self) -> Result<(), MsgError> {
        let modelled = self.bytes(self.nominal_elements);
        if let Some((lo, hi)) = self.anchor_spread()
            && !(lo..=hi).contains(&modelled)
        {
            return Err(MsgError::OutsideAnchorSpread {
                entry: self.key(),
                modelled,
                low: lo,
                high: hi,
            });
        }
        for a in self.anchors {
            let bound = a.payload_bytes();
            match a.kind {
                AnchorKind::UpperBound if modelled > bound => {
                    return Err(MsgError::OutsideAnchorSpread {
                        entry: self.key(),
                        modelled,
                        low: 0,
                        high: bound,
                    });
                }
                AnchorKind::LowerBound if modelled < bound => {
                    return Err(MsgError::OutsideAnchorSpread {
                        entry: self.key(),
                        modelled,
                        low: bound,
                        high: u32::MAX,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The table.
///
/// Ordered by message type then profile, and looked up linearly: it has eight rows and is
/// consulted once per generated message, so an index would cost more than it saves — and a
/// slice keeps the whole model readable in one screen, which is the point.
pub const TABLE: &[SizeEntry] = &[
    SizeEntry {
        ty: MsgType::Spat,
        profile: ContentProfile::Minimal,
        base_b: 17,
        per_element_b: 4,
        element: "movement state",
        nominal_elements: 8,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
    },
    SizeEntry {
        ty: MsgType::Spat,
        profile: ContentProfile::Typical,
        base_b: 17,
        per_element_b: 10,
        element: "movement state",
        nominal_elements: 8,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
    },
    SizeEntry {
        ty: MsgType::Spat,
        profile: ContentProfile::Rich,
        base_b: 17,
        per_element_b: 16,
        element: "movement state",
        nominal_elements: 12,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
    },
    SizeEntry {
        ty: MsgType::Map,
        profile: ContentProfile::Typical,
        base_b: 40,
        per_element_b: 40,
        element: "lane",
        nominal_elements: 12,
        anchors: &[Anchor {
            bytes: 2_302,
            kind: AnchorKind::UpperBound,
            scope: AnchorScope::WithCertificateEnvelopeAndHeaders,
            source: "CTI 4501 §4.3.3.1.3.1 — MAP ceiling with signature, certificate and header",
        }],
        status: EntryStatus::SingleAnchor,
        superseded_by: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
    },
    SizeEntry {
        ty: MsgType::Map,
        profile: ContentProfile::Rich,
        base_b: 40,
        per_element_b: 56,
        element: "lane",
        nominal_elements: 24,
        anchors: &[Anchor {
            bytes: 2_302,
            kind: AnchorKind::UpperBound,
            scope: AnchorScope::WithCertificateEnvelopeAndHeaders,
            source: "CTI 4501 §4.3.3.1.3.1 — MAP ceiling with signature, certificate and header",
        }],
        status: EntryStatus::SingleAnchor,
        superseded_by: Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
    },
    SizeEntry {
        ty: MsgType::Psm,
        profile: ContentProfile::Typical,
        base_b: 27,
        per_element_b: 8,
        element: "path-history point",
        nominal_elements: 0,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Srm,
        profile: ContentProfile::Typical,
        base_b: 20,
        per_element_b: 10,
        element: "signal request",
        nominal_elements: 1,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Ssm,
        profile: ContentProfile::Typical,
        base_b: 16,
        per_element_b: 12,
        element: "signal status",
        nominal_elements: 1,
        anchors: &[],
        status: EntryStatus::DerivedNoAnchor,
        superseded_by: None,
    },
];

/// Looks the row up, falling back to the typical profile when a message has no row for the
/// profile asked for.
///
/// The fallback is deliberate and narrow: a scenario that asks for a `rich` SRM should get
/// the SRM it has a model for rather than an error that stops the run, and the returned
/// entry's own `profile` field says which row answered.
pub fn lookup(ty: MsgType, profile: ContentProfile) -> Option<&'static SizeEntry> {
    TABLE
        .iter()
        .find(|e| e.ty == ty && e.profile == profile)
        .or_else(|| {
            TABLE
                .iter()
                .find(|e| e.ty == ty && e.profile == ContentProfile::Typical)
        })
}

/// The message types this codec **claims**.
///
/// SPaT and MAP are no longer among them: [`crate::j2735::infra::J2735InfraCodec`] encodes
/// both for real, and two codecs claiming one type would make a run's sizes depend on
/// which the engine resolved first (02-architecture.md §6). Their rows stay in [`TABLE`],
/// marked [`SizeEntry::superseded_by`] and still reachable through [`lookup`], because a
/// retired model value is evidence about the runs that used it.
pub const TYPES: [MsgType; 3] = [MsgType::Psm, MsgType::Srm, MsgType::Ssm];

/// The size-model codec: exact sizes, placeholder bytes.
#[derive(Debug, Clone)]
pub struct J2735SizeCodec {
    card: ModelCard,
}

impl Default for J2735SizeCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl J2735SizeCodec {
    /// Builds the codec and its card.
    pub fn new() -> Self {
        Self { card: card() }
    }

    /// The modelled payload size for a request.
    pub fn size_of(&self, request: &SizeRequest) -> Result<u32, CodecError> {
        lookup(request.ty, request.profile)
            .map(|e| e.bytes(request.elements))
            .ok_or_else(|| CodecError::Unsupported {
                codec: J2735_SIZE_MODEL_ID.to_string(),
                ty: request.ty,
            })
    }
}

fn card() -> ModelCard {
    let mut card = ModelCard::new(
        J2735_SIZE_MODEL_ID,
        Family::Codec,
        "1.0.0",
        "Validated size model for the SAE J2735 messages the engine does not really \
         encode (SPaT, MAP, PSM, SRM, SSM): exact modelled size, placeholder bytes.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![v2xw_core::card::Equation {
        notes: Some(
            "n is the message's variable element count: movement states for a SPaT, lanes \
             for a MAP, path-history points for a PSM, requests for an SRM, statuses for an \
             SSM."
                .to_string(),
        ),
        ..v2xw_core::card::Equation::new(
            "size",
            "bytes(type, profile, n) = base(type, profile) + n * increment(type, profile)",
        )
    }];

    for entry in TABLE {
        let (source, calibration) = match entry.status {
            EntryStatus::DerivedNoAnchor => (
                Source::todo_calibrate(format!(
                    "no published size for {} at the {} profile (04-models.md §8.2 records \
                     \"SPaT typical: none found\" and \"PSM: none\"); the value is derived \
                     from the J2735 2024-09 ASN.1 structure",
                    entry.ty, entry.profile
                )),
                Some(
                    "Encode a representative message with the real SAE module once imported \
                     (04-models.md §8.3's build-time importer), or with asn1c on a machine \
                     that holds an SAE licence, and record the measured bytes. Until then the \
                     conformance kit's `size_model == uper_len` assertion cannot run for this \
                     row."
                        .to_string(),
                ),
            ),
            EntryStatus::SingleAnchor | EntryStatus::LiteratureChecked => (
                Source::new(
                    SourceKind::Standard,
                    entry
                        .anchors
                        .first()
                        .map(|a| a.source)
                        .unwrap_or("see the size-model table"),
                ),
                None,
            ),
        };
        let mut parameter = Parameter::new(
            format!("{}_base_b", card_param_stem(entry)),
            "byte",
            serde_json::json!(entry.base_b),
            source.clone(),
        );
        parameter.calibration = calibration.clone();
        card.parameters.push(parameter);

        let mut increment = Parameter::new(
            format!("{}_per_{}_b", card_param_stem(entry), slug(entry.element)),
            "byte",
            serde_json::json!(entry.per_element_b),
            source,
        );
        increment.calibration = calibration;
        card.parameters.push(increment);
    }

    card.assumptions = vec![
        "Sizes are ASN.1 UPER payload bytes including the J2735 `MessageFrame` wrapper \
         (4 B: a 15-bit messageId plus the open-type length determinant), and excluding the \
         1609.2 envelope and every layer below it."
            .to_string(),
        "Anchors published at a wider layer scope are reduced to payload scope with the \
         overheads of 04-models.md §9.1 and §9.3 before they are compared."
            .to_string(),
    ];
    // The byte-exactness statement comes from crate::evidence, the one table that says
    // which payloads are real bytes and which are a fill pattern, so this card cannot
    // describe a message differently from the rest of the crate.
    card.limitations = crate::evidence::card_statement(J2735_SIZE_MODEL_ID);
    card.limitations.push(
        "The bytes this codec returns are a fill pattern. Anything that inspects a payload \
         must branch on Encoded::size_source first (invariant I-S2), and anything that \
         reports a size must not call it byte-exact."
            .to_string(),
    );
    card.limitations.push(
        "Five of the eight rows have no published anchor at all, so their tolerance is \
         undefined rather than zero, and they are marked todo-calibrate with the plan \
         04-models.md §8.4 prescribes."
            .to_string(),
    );
    card.limitations.push(
        "The spat/* and map/* rows are RETIRED: codec/uper/j2735-spat-map now encodes both \
         messages for real, so this codec no longer claims either type. The rows are kept \
         because a run recorded before that encoder existed has to stay interpretable, and \
         because they cross-check the encoder — but a size taken from them today is a \
         historical number, not this engine's answer."
            .to_string(),
    );
    card.ignores = vec![
        "Regional extensions entirely. They are the reason J2735 cannot be really encoded \
         here (build decision D2), and a deployment that uses them sends larger messages \
         than this model reports."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "SAE J2735 SET_202409 ASN.1 — structure only; the modules are not redistributable \
             and are not in this repository (build decision D3)",
        ),
        Source::new(
            SourceKind::Standard,
            "CTI 4501 v01.01 §4.3.3.1.3.1 — MAP ceiling 2 302 B with signature, certificate \
             and header",
        ),
        Source::new(
            SourceKind::Paper,
            "C2C-CC TR 2052 §3.1 — path history measured at 8-9 B per entry",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Standard,
            "CTI 4501 v01.01 §4.3.3.1.3.1 (MAP ceiling)",
        )],
        tests: vec![
            "size_model::tests::every_entry_satisfies_its_own_anchors".to_string(),
            "size_model::tests::an_unanchored_entry_admits_it".to_string(),
        ],
    };
    card
}

fn card_param_stem(entry: &SizeEntry) -> String {
    format!("{}_{}", entry.ty, entry.profile)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

impl Model for J2735SizeCodec {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MessageCodec for J2735SizeCodec {
    fn message_types(&self) -> &[MsgType] {
        &TYPES
    }

    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError> {
        match msg {
            Message::Modeled(request) => Ok(Encoded::size_model(self.size_of(request)?, VERSION)),
            other => Err(CodecError::Unsupported {
                codec: J2735_SIZE_MODEL_ID.to_string(),
                ty: other.msg_type(),
            }),
        }
    }

    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError> {
        if !self.supports(t) {
            return Err(CodecError::Unsupported {
                codec: J2735_SIZE_MODEL_ID.to_string(),
                ty: t,
            });
        }
        Err(CodecError::PlaceholderBytes {
            ty: t,
            version: VERSION,
            len: bytes.len(),
        })
    }
}

/// Every codec this crate registers, in a fixed order.
///
/// The order is the order a registry should see them in, and it is fixed because a
/// registry that iterated a hash map would make the manifest's model list depend on
/// allocation addresses.
pub fn codecs() -> Vec<Box<dyn MessageCodec>> {
    vec![
        Box::new(crate::codec::EtsiUperCodec::new()),
        Box::new(crate::j2735::J2735BsmCodec::new()),
        Box::new(crate::j2735::infra::J2735InfraCodec::new()),
        Box::new(J2735SizeCodec::new()),
        Box::new(crate::etsi_size::EtsiSizeCodec::new()),
    ]
}

/// Sanity check used by the registry and by the crate's own tests: no two codecs claim the
/// same message type.
///
/// A type claimed twice is not a style problem — it means a run's sizes depend on which
/// codec the engine happened to resolve first, which is exactly the class of
/// non-determinism 02-architecture.md §6 exists to prevent.
pub fn assert_no_overlapping_claims() -> Result<(), String> {
    let mut claimed: Vec<(MsgType, String)> = Vec::new();
    for codec in codecs() {
        for ty in codec.message_types() {
            if let Some((_, other)) = claimed.iter().find(|(t, _)| t == ty) {
                return Err(format!(
                    "{ty} is claimed by both `{other}` and `{}`",
                    codec.id()
                ));
            }
            claimed.push((*ty, codec.id().to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::SizeSource;

    #[test]
    fn the_card_validates() {
        J2735SizeCodec::new()
            .card()
            .validate()
            .expect("card validates, including rule R1 on todo-calibrate parameters");
    }

    /// Invariant I-S2: a model value must fall inside the spread of the cited anchors.
    #[test]
    fn every_entry_satisfies_its_own_anchors() {
        for entry in TABLE {
            entry
                .check()
                .unwrap_or_else(|e| panic!("{}: {e}", entry.key()));
        }
    }

    /// 04-models.md §8.4 says an entry with no anchor is `TODO: calibrate` with a plan.
    /// This is that rule, enforced.
    #[test]
    fn an_unanchored_entry_admits_it() {
        let card = J2735SizeCodec::new().card().clone();
        for entry in TABLE {
            let has_point_or_bound = !entry.anchors.is_empty();
            match entry.status {
                EntryStatus::DerivedNoAnchor => assert!(
                    !has_point_or_bound,
                    "{} claims no anchor but carries {}",
                    entry.key(),
                    entry.anchors.len()
                ),
                EntryStatus::SingleAnchor => assert_eq!(entry.anchors.len(), 1, "{}", entry.key()),
                EntryStatus::LiteratureChecked => {
                    assert!(entry.anchors.len() >= 2, "{}", entry.key())
                }
            }
        }
        // Every unanchored row's card parameters must be todo-calibrate with a plan, which
        // ModelCard::validate enforces — so the interesting assertion is that they exist.
        let todo: Vec<&str> = card.todo_calibrate().map(|p| p.name.as_str()).collect();
        assert!(
            todo.iter().any(|n| n.starts_with("spat_")),
            "SPaT has no published size, so its rows must be todo-calibrate: {todo:?}"
        );
        assert!(todo.iter().any(|n| n.starts_with("psm_")), "{todo:?}");
        assert!(
            !todo.iter().any(|n| n.starts_with("map_")),
            "MAP has the CTI 4501 ceiling, so it is not uncalibrated: {todo:?}"
        );
    }

    #[test]
    fn anchors_are_reduced_to_payload_scope_before_comparison() {
        let a = Anchor {
            bytes: 2_302,
            kind: AnchorKind::UpperBound,
            scope: AnchorScope::WithCertificateEnvelopeAndHeaders,
            source: "CTI 4501",
        };
        assert_eq!(a.payload_bytes(), 2_302 - (87 + 80) - 5);
    }

    #[test]
    fn the_codec_produces_exact_sizes_and_refuses_to_decode_them() {
        let codec = J2735SizeCodec::new();
        let request = SizeRequest::typical(MsgType::Spat, 8);
        let encoded = codec.encode(&Message::Modeled(request)).expect("encodes");
        assert_eq!(encoded.size, 17 + 8 * 10);
        assert_eq!(encoded.size_source, SizeSource::SizeModel(VERSION));
        assert!(!encoded.is_real());

        let err = codec.decode(&encoded.bytes, MsgType::Spat).unwrap_err();
        assert!(
            matches!(err, CodecError::PlaceholderBytes { .. }),
            "decoding a placeholder must say so: {err}"
        );
    }

    #[test]
    fn an_unknown_profile_falls_back_to_typical_and_says_so() {
        let entry = lookup(MsgType::Srm, ContentProfile::Rich).expect("falls back");
        assert_eq!(entry.profile, ContentProfile::Typical);
        assert_eq!(entry.ty, MsgType::Srm);
        assert!(lookup(MsgType::Bsm, ContentProfile::Typical).is_none());
    }

    #[test]
    fn the_two_codecs_do_not_claim_the_same_type() {
        assert_no_overlapping_claims().expect("no overlap");
    }

    #[test]
    fn sizes_are_monotonic_in_the_element_count() {
        for entry in TABLE {
            let mut previous = entry.bytes(0);
            for n in 1..64 {
                let now = entry.bytes(n);
                assert!(now >= previous, "{} is not monotonic", entry.key());
                previous = now;
            }
        }
    }

    /// A retired row must not be claimed, and must still be sizable.
    ///
    /// Both halves matter. The first is what keeps `assert_no_overlapping_claims` true
    /// once the real encoder is registered; the second is what keeps an old run's sizes
    /// interpretable.
    #[test]
    fn a_superseded_row_is_neither_claimed_nor_deleted() {
        let codec = J2735SizeCodec::new();
        for ty in [MsgType::Spat, MsgType::Map] {
            assert!(
                !codec.supports(ty),
                "{ty} is encoded for real now and must not be claimed by the size model"
            );
            let entry = lookup(ty, ContentProfile::Typical)
                .unwrap_or_else(|| panic!("{ty}'s retired row must stay in the table"));
            assert_eq!(
                entry.superseded_by,
                Some(crate::j2735::infra::J2735_INFRA_CODEC_ID),
                "{ty}"
            );
            assert!(entry.bytes(entry.nominal_elements) > 0, "{ty}");
        }
        for ty in TYPES {
            let entry = lookup(ty, ContentProfile::Typical).expect("a claimed row");
            assert_eq!(entry.superseded_by, None, "{ty} is still modelled");
        }
    }

    /// The retired SPaT row and the encoder that replaced it must agree to within a few
    /// bytes, or one of them is wrong.
    ///
    /// This is the only independent check the SPaT encoder has on this machine: the row's
    /// base and increment were derived from the ASN.1 by a pass that could read it, and the
    /// encoder was written by a pass that could not (the modules are git-ignored and absent
    /// — build decision D3). They are two derivations of the same structure, and a gross
    /// disagreement means a preamble or a determinant is wrong somewhere.
    ///
    /// Measured, for a SPaT of eight movement states each carrying one timed event: the
    /// encoder produces 85 B of payload and 88 B inside a `MessageFrame`, against the row's
    /// 17 + 8 x 10 = 97 B (which includes the 4 B frame). The encoder is 9 B smaller, and
    /// the two known reasons are documented: the row counted 5 bits for `eventState` where
    /// the encoder writes 4, and it charged a flat 4 B for the `MessageFrame` where the real
    /// wrapper costs 3 B below 128 octets.
    #[test]
    fn the_real_spat_encoder_lands_near_the_row_it_retired() {
        use crate::j2735::spat::{
            IntersectionReferenceId, IntersectionState, IntersectionStatus, MovementEvent,
            MovementPhaseState, MovementState, Spat, TimeChangeDetails, time_mark,
        };

        let row = lookup(MsgType::Spat, ContentProfile::Typical).expect("the retired row");
        let elements = row.nominal_elements;
        let states: Vec<MovementState> = (0..elements)
            .map(|i| {
                MovementState::current(
                    (i + 1) as u8,
                    MovementEvent::timed(
                        MovementPhaseState::ProtectedMovementAllowed,
                        TimeChangeDetails::fixed(time_mark(0.0), time_mark(27.5)),
                    ),
                )
            })
            .collect();
        let spat = Spat::one(IntersectionState {
            id: IntersectionReferenceId::new(1),
            revision: 1,
            status: IntersectionStatus::FIXED_TIME_OPERATION,
            moy: None,
            time_stamp: None,
            states,
        });

        let modelled = row.bytes(elements);
        let real = crate::j2735::spat::encode_message_frame(&spat)
            .expect("encodes")
            .size;
        let difference = modelled.abs_diff(real);
        assert!(
            difference <= 16,
            "the retired SPaT row says {modelled} B and the encoder says {real} B for \
             {elements} movement states; a gap that large means one of the two readings of \
             the ASN.1 is wrong, not that the model is coarse"
        );
    }

    /// A MAP big enough to break the CTI 4501 ceiling must be caught by the row's own
    /// check, not slip through. This exercises the checker, not the table.
    #[test]
    fn the_checker_catches_a_row_that_exceeds_an_upper_bound() {
        let bad = SizeEntry {
            per_element_b: 400,
            ..*TABLE
                .iter()
                .find(|e| e.ty == MsgType::Map && e.profile == ContentProfile::Typical)
                .unwrap()
        };
        let err = bad
            .check()
            .expect_err("12 lanes x 400 B is over the ceiling");
        assert!(err.to_string().contains("map/typical"), "{err}");
    }
}
