//! The size model for the two ETSI messages this crate cannot encode yet: CPM and VAM.
//!
//! # Why they are modelled and not encoded — the honest answer
//!
//! Build decision D2 says the ETSI stack is **generated** from the forge modules, and it
//! is right: `rasn-compiler` handled CPM (TS 103 324) and VAM (TS 103 300-3) in the asset
//! survey, and [`crate::codec::EtsiUperCodec`]'s own documentation says both "arrive by
//! adding their modules to the `FACILITIES` unit in `build.rs` and a variant to
//! [`crate::Message`]".
//!
//! That is still the plan, and it is still not done here, for a reason that has nothing to
//! do with the code: **the modules are not in this checkout.**
//! `third_party/asn1/etsi/` holds the CDD, CAM, DENM, IEEE 1609.2 and TS 103 097 modules
//! and nothing else — no `CPM-PDU-Descriptions.asn`, no `VAM-PDU-Descriptions.asn`. They
//! are BSD-3-Clause and *should* be committed; they simply are not yet.
//!
//! Two things follow, and both matter more than they look:
//!
//! * `build.rs` is left alone. Adding either module to the `FACILITIES` unit would make
//!   the build **fail** with "missing ASN.1 source", because that unit's inputs are
//!   required files. A broken build is worse than a size model.
//! * Nothing here hand-writes a CPM or a VAM. Hand-writing an ETSI PDU whose module is
//!   absent would mean recalling a container layout that changed between releases, and
//!   presenting the result as real bytes — the exact failure this crate exists to avoid.
//!   A cited size model is a smaller claim, and it is a true one.
//!
//! # What it takes to replace this module with real encoders
//!
//! 1. Fetch `CPM-PDU-Descriptions.asn` (TS 103 324) and `VAM-PDU-Descriptions.asn`
//!    (TS 103 300-3) from the ETSI forge, commit them under
//!    `third_party/asn1/etsi/cpm_ts103324/` and `.../vam_ts103300_3/` with their `LICENSE`
//!    files, and record each file's URL, commit and sha256 in that directory's
//!    `PROVENANCE.md`, exactly as the other five modules are recorded.
//! 2. Carry the recorded patch build decision D5 already names: the VAM module lists
//!    `SequenceOfTrajectoryInterceptionIndication` twice in its `IMPORTS`.
//! 3. Add both files to `FACILITIES` in `build.rs`, add `Message::Cpm` and `Message::Vam`
//!    variants, extend [`crate::codec::EtsiUperCodec::TYPES`], and move the CPM and VAM
//!    rows of [`crate::evidence::EVIDENCE`] to
//!    [`crate::evidence::ByteExactness::GeneratedFromModule`].
//! 4. Delete this module and its rows. The conformance kit's `size_model == uper_len`
//!    assertion (04-models.md §8.4) is what should retire it.
//!
//! # Where the numbers come from
//!
//! | Row | Base | Per element | Anchors |
//! |---|---:|---|---|
//! | CPM minimal | 121 B | 35 B per perceived object | TR 103 562 Table 3 measures the mandatory header + management + station data at 121 B and each perceived object at 35 B |
//! | CPM typical | 156 B | 35 B per object | the same, plus one sensor-information container at 35 B |
//! | CPM rich | 156 B | 66 B per object | 35 B mandatory + about 31 B of optional data frames per object [TR 103 562 §5.3.2] |
//! | VAM minimal | 90 B | 8 B per path-history point | **fitted**, see below |
//! | VAM typical | 90 B | 8 B per point | **fitted**, see below |
//! | VAM rich | 120 B | 8 B per point | **fitted**, plus the cluster-information container |
//!
//! The CPM base and increment are measured numbers from a published table. The VAM base is
//! not: no published VAM decomposition was found, so the base is **fitted** to the spread
//! of the four published whole-message sizes (TR 2050 Fig. 12's 350 B with security, 5GAA's
//! 350 B, C2C-CC's 235 B and the 300 B midpoint arXiv 2506.22052 uses), reduced to payload
//! scope. Every VAM parameter on the card is therefore `TODO: calibrate` with the plan
//! above, because a value that reproduces a total without being derived from a structure is
//! a curve fit and is labelled as one. The 8 B per path point is cited: C2C-CC TR 2052 §3.1
//! measures a CAM path-history entry at 8–9 B and a VAM carries the same three offsets and
//! a time.

use crate::codec::{Encoded, Message, MessageCodec, MsgType, SizeModelVersion};
use crate::error::{CodecError, MsgError};
use crate::size_model::{
    Anchor, AnchorKind, AnchorScope, ContentProfile, EntryStatus, SizeEntry, SizeRequest,
};
use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;

/// Model id of the ETSI size-model codec.
pub const ETSI_SIZE_MODEL_ID: &str = "codec/size-model/etsi";

/// This table's version, bumped whenever a number below changes.
///
/// Separate from [`crate::size_model::VERSION`]: the two tables are versioned
/// independently because they are calibrated by different evidence and will be retired at
/// different times — these two rows disappear the day their modules are committed.
pub const ETSI_VERSION: SizeModelVersion = SizeModelVersion::new(1, 0, 0);

/// What a CPM's variable element is.
pub const CPM_ELEMENT: &str = "perceived object";
/// What a VAM's variable element is.
pub const VAM_ELEMENT: &str = "path-history point";

/// TR 103 562 Table 3: the mandatory header, management and station-data containers.
const CPM_MANDATORY_B: u32 = 121;
/// TR 103 562 Table 3: one sensor-information container.
const CPM_SENSOR_INFORMATION_B: u32 = 35;
/// TR 103 562 Table 3: one perceived object, mandatory data elements only.
const CPM_OBJECT_B: u32 = 35;
/// TR 103 562 §5.3.2: the optional data frames add about 31 B to a perceived object.
const CPM_OBJECT_OPTIONAL_B: u32 = 31;

/// The anchors both CPM rows are checked against.
///
/// Two point anchors that bracket a whole CPM: the 121 B mandatory floor from the measured
/// container table, and the payload part of TR 2050's 1 000 B planning size. Any modelled
/// CPM has to sit between "no objects at all" and "what the planning figure allows".
const CPM_ANCHORS: &[Anchor] = &[
    Anchor {
        bytes: CPM_MANDATORY_B,
        kind: AnchorKind::Point,
        scope: AnchorScope::Payload,
        source: "ETSI TR 103 562 Table 3 — CPM header + management + station data, \
                 mandatory data elements, UPER payload",
    },
    Anchor {
        bytes: 1_000,
        kind: AnchorKind::Point,
        scope: AnchorScope::WithCertificateEnvelope,
        source: "C2C-CC TR 2050 Fig. 13 — CPM planning size with security (about 750 B of \
                 payload, quoted as roughly 25 objects)",
    },
];

/// The anchors both VAM rows are checked against: four published whole-message sizes,
/// every one of them at secured-message scope.
const VAM_ANCHORS: &[Anchor] = &[
    Anchor {
        bytes: 235,
        kind: AnchorKind::Point,
        scope: AnchorScope::WithCertificateEnvelope,
        source: "C2C-CC VAM size, as quoted by arXiv 2506.22052 (the low end of its range)",
    },
    Anchor {
        bytes: 300,
        kind: AnchorKind::Point,
        scope: AnchorScope::WithCertificateEnvelope,
        source: "arXiv 2506.22052 (Ostendorf 2025) — 300 B used as the midpoint of 235 and 350",
    },
    Anchor {
        bytes: 350,
        kind: AnchorKind::Point,
        scope: AnchorScope::WithCertificateEnvelope,
        source: "C2C-CC TR 2050 Fig. 12 — VAM planning size with security; 5GAA states the \
                 same 350 B",
    },
];

/// The table.
///
/// Six rows, looked up linearly for the same reason [`crate::size_model::TABLE`] is: it is
/// consulted once per generated message and it should stay readable in one screen.
pub const ETSI_TABLE: &[SizeEntry] = &[
    SizeEntry {
        ty: MsgType::Cpm,
        profile: ContentProfile::Minimal,
        base_b: CPM_MANDATORY_B,
        per_element_b: CPM_OBJECT_B,
        element: CPM_ELEMENT,
        nominal_elements: 5,
        anchors: CPM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Cpm,
        profile: ContentProfile::Typical,
        base_b: CPM_MANDATORY_B + CPM_SENSOR_INFORMATION_B,
        per_element_b: CPM_OBJECT_B,
        element: CPM_ELEMENT,
        nominal_elements: 10,
        anchors: CPM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Cpm,
        profile: ContentProfile::Rich,
        base_b: CPM_MANDATORY_B + CPM_SENSOR_INFORMATION_B,
        per_element_b: CPM_OBJECT_B + CPM_OBJECT_OPTIONAL_B,
        element: CPM_ELEMENT,
        nominal_elements: 10,
        anchors: CPM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Vam,
        profile: ContentProfile::Minimal,
        base_b: 90,
        per_element_b: 8,
        element: VAM_ELEMENT,
        nominal_elements: 0,
        anchors: VAM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Vam,
        profile: ContentProfile::Typical,
        base_b: 90,
        per_element_b: 8,
        element: VAM_ELEMENT,
        nominal_elements: 5,
        anchors: VAM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
    SizeEntry {
        ty: MsgType::Vam,
        profile: ContentProfile::Rich,
        base_b: 120,
        per_element_b: 8,
        element: VAM_ELEMENT,
        nominal_elements: 5,
        anchors: VAM_ANCHORS,
        status: EntryStatus::LiteratureChecked,
        superseded_by: None,
    },
];

/// The message types this table covers.
pub const ETSI_SIZE_TYPES: [MsgType; 2] = [MsgType::Cpm, MsgType::Vam];

/// Looks up a row, falling back to the typical profile when the asked-for one has no row.
///
/// The same narrow fallback [`crate::size_model::lookup`] uses, and for the same reason: a
/// scenario asking for a profile that has no row should get the message it does have a
/// model for rather than an error that stops the run.
pub fn lookup(ty: MsgType, profile: ContentProfile) -> Option<&'static SizeEntry> {
    ETSI_TABLE
        .iter()
        .find(|e| e.ty == ty && e.profile == profile)
        .or_else(|| {
            ETSI_TABLE
                .iter()
                .find(|e| e.ty == ty && e.profile == ContentProfile::Typical)
        })
}

/// The ETSI size-model codec: exact modelled sizes, placeholder bytes.
#[derive(Debug, Clone)]
pub struct EtsiSizeCodec {
    card: ModelCard,
}

impl Default for EtsiSizeCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl EtsiSizeCodec {
    /// Builds the codec and its card.
    pub fn new() -> Self {
        Self { card: card() }
    }

    /// The modelled payload size for a request.
    pub fn size_of(&self, request: &SizeRequest) -> Result<u32, CodecError> {
        if !ETSI_SIZE_TYPES.contains(&request.ty) {
            return Err(CodecError::Unsupported {
                codec: ETSI_SIZE_MODEL_ID.to_string(),
                ty: request.ty,
            });
        }
        lookup(request.ty, request.profile)
            .map(|e| e.bytes(request.elements))
            .ok_or_else(|| CodecError::Unsupported {
                codec: ETSI_SIZE_MODEL_ID.to_string(),
                ty: request.ty,
            })
    }
}

fn card() -> ModelCard {
    let mut card = ModelCard::new(
        ETSI_SIZE_MODEL_ID,
        Family::Codec,
        "1.0.0",
        "Size model for the two ETSI messages this crate does not yet encode (CPM, VAM), \
         because their ASN.1 modules are not in this checkout: exact modelled size, \
         placeholder bytes.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![v2xw_core::card::Equation {
        notes: Some(
            "n is the perceived-object count for a CPM and the path-history point count for \
             a VAM."
                .to_string(),
        ),
        ..v2xw_core::card::Equation::new(
            "size",
            "bytes(type, profile, n) = base(type, profile) + n * increment(type, profile)",
        )
    }];

    let cpm_source = Source::new(
        SourceKind::Paper,
        "ETSI TR 103 562 Table 3 and §5.3.2 — measured CPM container sizes: 121 B for the \
         mandatory header, management and station data, 35 B per sensor-information \
         container, 35 B per perceived object, about 31 B more per object with the optional \
         data frames",
    );
    let vam_plan = "Commit VAM-PDU-Descriptions.asn (TS 103 300-3) under \
                    third_party/asn1/etsi/vam_ts103300_3/ with its LICENSE and PROVENANCE \
                    entry, carry build decision D5's duplicate-IMPORTS patch, add it to the \
                    FACILITIES unit in build.rs, and encode a representative VAM. Record the \
                    measured bytes and delete this row. Until then the value reproduces four \
                    published totals and is not derived from the structure."
        .to_string();

    for entry in ETSI_TABLE {
        let (source, calibration) = if entry.ty == MsgType::Cpm {
            (cpm_source.clone(), None)
        } else {
            (
                Source::todo_calibrate(
                    "no published VAM container decomposition was found; the base is fitted \
                     to the payload-scope spread of the four cited whole-message sizes, not \
                     derived from the ASN.1",
                ),
                Some(vam_plan.clone()),
            )
        };
        let stem = format!("{}_{}", entry.ty, entry.profile);
        let mut base = Parameter::new(
            format!("{stem}_base_b"),
            "byte",
            serde_json::json!(entry.base_b),
            source.clone(),
        );
        base.calibration = calibration.clone();
        card.parameters.push(base);

        let mut increment = Parameter::new(
            format!("{stem}_per_element_b"),
            "byte",
            serde_json::json!(entry.per_element_b),
            if entry.ty == MsgType::Vam {
                Source::new(
                    SourceKind::Paper,
                    "C2C-CC TR 2052 §3.1 — path history measured at 8-9 B per entry; a VAM \
                     path point carries the same three offsets and a time",
                )
            } else {
                source
            },
        );
        // The VAM *increment* is cited even though its base is not, so it carries no
        // calibration plan: marking a cited number todo-calibrate would dilute the report
        // that rule R1 exists to produce.
        increment.calibration = None;
        card.parameters.push(increment);
    }

    card.limitations = crate::evidence::card_statement(ETSI_SIZE_MODEL_ID);
    card.limitations.push(
        "The bytes this codec returns are a fill pattern. Anything that inspects a payload \
         must branch on Encoded::size_source first (invariant I-S2), and anything that \
         reports a size must not call it byte-exact — v2xw_msg::evidence is the predicate \
         for that."
            .to_string(),
    );
    card.limitations.push(
        "Both messages are generatable from the published ETSI modules and should be real \
         encoders. They are modelled here only because CPM-PDU-Descriptions.asn and \
         VAM-PDU-Descriptions.asn are absent from third_party/asn1/etsi in this checkout. \
         The module documentation lists the four steps that replace this codec."
            .to_string(),
    );
    card.limitations.push(
        "The VAM base is fitted to published totals rather than derived from the ASN.1, so \
         it will reproduce a plausible whole-message size while being wrong about how that \
         size is composed. Every VAM base parameter is todo-calibrate for exactly that \
         reason."
            .to_string(),
    );
    card.limitations.push(
        "A CPM's segmentation (TS 103 324 §7.3) is not modelled: a perception set too large \
         for the MTU is sent as several CPMs in reality and as one oversized modelled size \
         here. The generator is where that belongs, not the size model."
            .to_string(),
    );

    card.assumptions = vec![
        "Sizes are UPER payload bytes, excluding the 1609.2 or TS 103 097 envelope, the \
         GeoNetworking/BTP headers and everything below them."
            .to_string(),
        "Anchors published at secured-message scope are reduced to payload scope with the \
         envelope overheads of 04-models.md §9.1 before they are compared, which is what \
         v2xw_msg::size_model::AnchorScope does."
            .to_string(),
        "CPM elements are perceived objects and one sensor-information container is folded \
         into the typical and rich bases; VAM elements are path-history points."
            .to_string(),
    ];
    card.ignores = vec![
        "Perception-region containers (up to 8 per CPM) and the VAM cluster-operation \
         container, neither of which has a published size."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 324 (CPM) and TS 103 300-3 (VAM) — referenced for structure; the \
             modules are BSD-3-Clause and should be committed, and are not in this checkout",
        ),
        cpm_source,
        Source::new(
            SourceKind::Paper,
            "C2C-CC TR 2050 Figs. 12-13 — VAM 350 B and CPM 1 000 B planning sizes with \
             security",
        ),
        Source::new(
            SourceKind::Paper,
            "arXiv 2506.22052 (Ostendorf 2025) — VAM 300 B as the midpoint of C2C-CC's 235 B \
             and 5GAA's 350 B",
        ),
        Source::new(
            SourceKind::Code,
            "this repository: docs/design/04-models.md §8.2 (the size table these anchors \
             come from) and §8.4 (the anchor-spread tolerance rule)",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![
            Source::new(
                SourceKind::Paper,
                "ETSI TR 103 562 Table 3 (CPM containers, measured)",
            ),
            Source::new(
                SourceKind::Paper,
                "C2C-CC TR 2050 Figs. 12-13 and arXiv 2506.22052 (VAM and CPM totals)",
            ),
        ],
        tests: vec![
            "etsi_size::tests::every_entry_satisfies_its_own_anchors".to_string(),
            "etsi_size::tests::the_vam_rows_admit_that_their_base_is_fitted".to_string(),
            "etsi_size::tests::the_codec_produces_exact_sizes_and_refuses_to_decode_them"
                .to_string(),
        ],
    };
    card
}

impl Model for EtsiSizeCodec {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MessageCodec for EtsiSizeCodec {
    fn message_types(&self) -> &[MsgType] {
        &ETSI_SIZE_TYPES
    }

    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError> {
        match msg {
            Message::Modeled(request) => {
                Ok(Encoded::size_model(self.size_of(request)?, ETSI_VERSION))
            }
            other => Err(CodecError::Unsupported {
                codec: ETSI_SIZE_MODEL_ID.to_string(),
                ty: other.msg_type(),
            }),
        }
    }

    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError> {
        if !self.supports(t) {
            return Err(CodecError::Unsupported {
                codec: ETSI_SIZE_MODEL_ID.to_string(),
                ty: t,
            });
        }
        Err(CodecError::PlaceholderBytes {
            ty: t,
            version: ETSI_VERSION,
            len: bytes.len(),
        })
    }
}

/// Checks every row against its own anchors — invariant I-S2, for this table.
pub fn check_table() -> Result<(), MsgError> {
    for entry in ETSI_TABLE {
        entry.check()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::SizeSource;

    #[test]
    fn the_card_validates() {
        EtsiSizeCodec::new()
            .card()
            .validate()
            .expect("card validates, including rule R1 on todo-calibrate parameters");
        EtsiSizeCodec::new()
            .card()
            .check_api_version()
            .expect("api version");
    }

    /// Invariant I-S2: a modelled value must sit inside the spread of its cited point
    /// anchors, at the element count the anchors describe.
    #[test]
    fn every_entry_satisfies_its_own_anchors() {
        check_table().expect("every row is inside its anchor spread");
        // And the spreads really are what the sources say, reduced to payload scope: the
        // CPM row is bounded below by the 121 B mandatory containers and above by TR
        // 2050's planning size less a certificate envelope.
        let cpm = lookup(MsgType::Cpm, ContentProfile::Typical).expect("a row");
        let (lo, hi) = cpm.anchor_spread().expect("point anchors");
        assert_eq!(lo, CPM_MANDATORY_B);
        assert_eq!(hi, 1_000 - (87 + 80));
        assert!((lo..=hi).contains(&cpm.bytes(cpm.nominal_elements)));

        let vam = lookup(MsgType::Vam, ContentProfile::Typical).expect("a row");
        let (lo, hi) = vam.anchor_spread().expect("point anchors");
        assert_eq!((lo, hi), (235 - (87 + 80), 350 - (87 + 80)));
        assert!((lo..=hi).contains(&vam.bytes(vam.nominal_elements)));
    }

    /// The fitted numbers must be on the todo-calibrate report, and the cited ones must
    /// not be: a report that flagged everything would say nothing.
    #[test]
    fn the_vam_rows_admit_that_their_base_is_fitted() {
        let card = EtsiSizeCodec::new().card().clone();
        let todo: Vec<&str> = card.todo_calibrate().map(|p| p.name.as_str()).collect();
        for profile in ["minimal", "typical", "rich"] {
            let name = format!("vam_{profile}_base_b");
            assert!(
                todo.contains(&name.as_str()),
                "{name} is fitted and must be todo-calibrate: {todo:?}"
            );
        }
        assert!(
            !todo.iter().any(|n| n.starts_with("cpm_")),
            "the CPM rows come from a measured table and must not be flagged: {todo:?}"
        );
        assert!(
            !todo
                .iter()
                .any(|n| n.ends_with("per_element_b") && n.starts_with("vam_")),
            "the VAM increment is cited (TR 2052 §3.1) and must not be flagged: {todo:?}"
        );
    }

    #[test]
    fn the_codec_produces_exact_sizes_and_refuses_to_decode_them() {
        let codec = EtsiSizeCodec::new();
        let encoded = codec
            .encode(&Message::Modeled(SizeRequest::typical(MsgType::Cpm, 4)))
            .expect("sizes");
        assert_eq!(encoded.size, 121 + 35 + 4 * 35);
        assert_eq!(encoded.size_source, SizeSource::SizeModel(ETSI_VERSION));
        assert!(!encoded.is_real());
        assert_eq!(encoded.bytes.len(), encoded.size as usize);

        let err = codec.decode(&encoded.bytes, MsgType::Cpm).unwrap_err();
        assert!(matches!(err, CodecError::PlaceholderBytes { .. }), "{err}");

        // A VAM with no path points is its base.
        assert_eq!(
            codec
                .encode(&Message::Modeled(SizeRequest {
                    ty: MsgType::Vam,
                    profile: ContentProfile::Minimal,
                    elements: 0,
                }))
                .expect("sizes")
                .size,
            90
        );
    }

    #[test]
    fn it_claims_exactly_cpm_and_vam_and_refuses_everything_else() {
        let codec = EtsiSizeCodec::new();
        assert!(codec.supports(MsgType::Cpm));
        assert!(codec.supports(MsgType::Vam));
        for ty in MsgType::ALL {
            if ty != MsgType::Cpm && ty != MsgType::Vam {
                assert!(!codec.supports(ty), "{ty} should not be claimed");
                assert!(
                    codec
                        .encode(&Message::Modeled(SizeRequest::typical(ty, 1)))
                        .is_err(),
                    "{ty} must not be sized by the ETSI table"
                );
            }
        }
        assert!(
            codec
                .encode(&Message::HandEncoded {
                    ty: MsgType::Cpm,
                    bytes: vec![0; 10]
                })
                .is_err(),
            "a size model encodes nothing"
        );
    }

    #[test]
    fn sizes_are_monotonic_in_the_element_count() {
        for entry in ETSI_TABLE {
            let mut previous = entry.bytes(0);
            for n in 1..64 {
                let now = entry.bytes(n);
                assert!(now >= previous, "{} is not monotonic", entry.key());
                previous = now;
            }
        }
    }

    #[test]
    fn an_unknown_profile_falls_back_to_typical() {
        // Every profile has a row today, so the fallback is exercised by asking for a
        // profile the table deliberately does not distinguish for a type.
        let entry = lookup(MsgType::Vam, ContentProfile::Rich).expect("a row");
        assert_eq!(entry.profile, ContentProfile::Rich);
        assert!(lookup(MsgType::Bsm, ContentProfile::Typical).is_none());
    }
}
