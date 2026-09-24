//! `v2xw-msg` — the message layer: what a node sends, when, and what it weighs on the wire.
//!
//! # Read this first: the bytes are not all real
//!
//! Build decision D2 divides the message set into tiers, and the whole crate is organised
//! around that division. Anything that consumes an encoded message must branch on
//! [`codec::Encoded::size_source`] before it treats the payload as data, because the
//! bottom tier produces no payload at all — and anything that *reports* a size must go one
//! step further and consult [`evidence`], because two tiers share one `size_source` and
//! only one of them has been checked against an independent implementation.
//!
//! | Tier | Messages | What [`codec::Encoded::bytes`] contains | `size_source` |
//! |---|---|---|---|
//! | **Real ASN.1, generated** | ETSI CAM, DENM, VAM ([`vam`]) (this crate); IEEE 1609.2 / TS 103 097 envelope and certificates (`v2xw-sec`, over [`sec_types`]) | the wire bytes, from `rasn` bindings generated at build time from the ETSI forge modules | [`codec::SizeSource::Uper`] / [`codec::SizeSource::Coer`] |
//! | **Hand-written, oracle-validated** | SAE J2735 BSM ([`j2735::bsm`]) | the wire bytes of the subset the simulator fills | [`codec::SizeSource::Uper`] |
//! | **Hand-written, not yet validated** | SAE J2735 SPaT ([`j2735::spat`]), MAP ([`j2735::map`]) and PSM ([`j2735::psm`]) | the wire bytes of the subset the simulator fills | [`codec::SizeSource::Uper`] |
//! | **Size model** | J2735 PSM, SRM, SSM ([`size_model`]); ETSI CPM, VAM ([`etsi_size`]) — the PSM and VAM rows are what the generic codec seam (and [`evidence`]) still register; the VRU device sends the real [`j2735::psm`] and [`vam`] bytes | **a fill pattern of exactly the modelled length** | [`codec::SizeSource::SizeModel`] |
//!
//! [`evidence`] is the machine-readable form of that table — one row per
//! [`codec::MsgType`], and the paragraph every model card's `limitations` starts with. A
//! number is only "byte-exact" if [`evidence::ByteExactness::is_byte_exact`] says so:
//! [`codec::SizeSource`] cannot tell you, because the second and third tiers share one
//! value and carry very different evidence.
//!
//! ## Why the middle and bottom tiers exist
//!
//! Not for want of trying. `rasn-compiler` 0.16 *does* generate Rust for all 43 SAE J2735
//! 2024-09 modules — and that Rust does not compile: 170 errors, essentially all from
//! `RegionalExtension {REG-EXT-ID-AND-TYPE : Set}`, a parameterised type over an information
//! object class that the compiler does not implement. Mechanically deleting the 55 optional
//! `regional` fields makes codegen succeed and still leaves 105 errors, so it is not a
//! one-line fix. On top of that, the J2735 ASN.1 carries an SAE licence forbidding
//! redistribution, so the modules are git-ignored and can never be committed (build
//! decision D3).
//!
//! The ETSI forge modules have neither problem: they are BSD-3-Clause, they are committed
//! under `third_party/asn1/etsi/` with their licences and a provenance record, and the Rust
//! generated from them compiles and round-trips. So the ETSI stack is genuinely encoded and
//! the US stack is not — and this crate never pretends otherwise.
//!
//! Two gaps in that story are worth stating at the top rather than in a module nobody
//! opens, because both are about *this checkout* rather than about the design:
//!
//! * `third_party/asn1/j2735/` **does not exist here** (it is git-ignored by D3), and
//!   neither does the `pycrate` oracle environment. The BSM codec was written and
//!   validated when both were present. The SPaT and MAP codecs were not: their structure
//!   is corroborated where an artefact in this repository corroborates it and recalled
//!   where nothing does, every recalled choice is a named constant, and their card says
//!   the bytes are unvalidated.
//! * `CPM-PDU-Descriptions.asn` is **not committed**, so the CPM is sized instead
//!   ([`etsi_size`], which lists the steps that fix it). `VAM-PDU-Descriptions.asn` was
//!   committed on 2026-09-23 and the VAM is generated ([`vam`]).
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | `MessageCodec`, `MsgType`, `Encoded`, `SizeSource` | [`codec`] | 03-interfaces.md §6 |
//! | Generated ETSI bindings (CDD, CAM, DENM) | [`asn1`] | TS 102 894-2, TS 103 900, TS 103 831 |
//! | Generated IEEE 1609.2 / TS 103 097 bindings, for `v2xw-sec` | [`sec_types`] | IEEE 1609.2, TS 103 097 |
//! | Which messages are byte-exact, which are not | [`evidence`] | build decision D2 |
//! | Building, encoding and decoding a CAM | [`cam`] | ETSI TS 103 900 |
//! | Building a DENM and running its lifecycle | [`denm`] | ETSI TS 103 831, EN 302 637-3 |
//! | `MessageGenerator`, the CAM trigger state machine, the BSM cadence | [`generator`] | 03-interfaces.md §6, 04-models.md §8.1 |
//! | The hand-written J2735 BSM codec and its UPER engine | [`j2735`] | SAE J2735 2024-09, ITU-T X.691 |
//! | The hand-written J2735 SPaT and MAP codecs | [`j2735::spat`], [`j2735::map`], [`j2735::infra`] | SAE J2735 2024-09, ITU-T X.691 |
//! | The validated J2735 size model (PSM, SRM, SSM) | [`size_model`] | 04-models.md §8.4 |
//! | The ETSI size model (CPM, VAM), and how to retire it | [`etsi_size`] | 04-models.md §8.2, §8.4 |
//! | Simulator quantities to CDD wire units | [`units`] | ETSI TS 102 894-2 |
//! | Errors | [`error`] | — |
//!
//! # Getting a CAM on the wire
//!
//! ```
//! use v2xw_msg::cam::{self, CamInput, ParticipantType};
//! use v2xw_msg::codec::{MessageCodec, Message, EtsiUperCodec, MsgType, SizeSource};
//! use v2xw_core::belief::PositionEstimate;
//! use v2xw_core::geo::GeoOrigin;
//! use v2xw_core::geom::{Dims, Vec3};
//! use v2xw_core::time::WallClock;
//!
//! let clock = WallClock::parse_rfc3339("2026-09-18T12:00:00Z")?;
//! let origin = GeoOrigin::new(40.7440, -73.9900, 0.0);
//!
//! let mut belief = PositionEstimate::no_fix(0);
//! belief.pos = Vec3::new(1_200.0, 800.0, 12.5);
//! belief.vel = Vec3::new(9.8, 9.8, 0.0);
//! belief.heading_rad = std::f64::consts::FRAC_PI_4;
//! belief.semi_major_m = 1.8;
//! belief.semi_minor_m = 1.1;
//! belief.fix = v2xw_core::belief::FixQuality::ThreeD;
//!
//! let input = CamInput::new(
//!     0x0A0B_0C0D,
//!     ParticipantType::PassengerCar,
//!     belief,
//!     origin,
//!     Dims::CAR,
//!     cam::timestamp_its(clock, 0)?,
//! );
//!
//! let codec = EtsiUperCodec::new();
//! let encoded = codec.encode(&Message::Cam(Box::new(cam::build_cam(&input)?)))?;
//! assert_eq!(encoded.size_source, SizeSource::Uper);   // real bytes
//! assert!(encoded.size > 0 && encoded.size < 200);
//!
//! // ...and back.
//! let Message::Cam(decoded) = codec.decode(&encoded.bytes, MsgType::Cam)? else { unreachable!() };
//! assert_eq!(decoded.header.station_id.0, 0x0A0B_0C0D);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Determinism
//!
//! * No standard-library transcendental. Every one goes through [`v2xw_core::math`]
//!   (ADR 0003). `round`, `ceil`, `floor` and `abs` are IEEE-754 exact operations and are
//!   used directly.
//! * Every float is quantised on its declared grid **before** it is scaled to a wire unit
//!   or compared against a trigger threshold (build decisions D9 and D10), so an encoded
//!   integer is a function of the quantised value rather than of an f64's last bit.
//! * Nothing here draws a random number. The one rule that needs randomness — the DENM
//!   keep-alive forwarding jitter — takes the draw as a parameter
//!   ([`denm::forwarding_delay`]) so it stays on the caller's own
//!   [`v2xw_core::rng::RngStream`].
//! * No `HashMap` iteration reaches an output: [`denm::DenmService::poll`] sorts by
//!   `actionId` and [`size_model::codecs`] returns a fixed order.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod asn1;
pub mod cam;
pub mod codec;
pub mod denm;
pub mod error;
pub mod etsi_size;
pub mod evidence;
pub mod generator;
pub mod j2735;
pub mod sec_types;
pub mod size_model;
pub mod units;
pub mod vam;

pub use codec::{
    ETSI_UPER_CODEC_ID, Encoded, EtsiUperCodec, Message, MessageCodec, MsgType, SizeModelVersion,
    SizeSource,
};
pub use error::{CodecError, MsgError, Result};
pub use etsi_size::{ETSI_SIZE_MODEL_ID, EtsiSizeCodec};
pub use evidence::{ByteExactness, EVIDENCE, MessageEvidence, byte_exactness};
pub use generator::{
    AppEvent, AppEventKind, BSM_GENERATOR_ID, BsmGenParams, BsmGenerator, CAM_GENERATOR_ID,
    CamDynamics, CamGenParams, CamGenerator, CamTriggerState, DccState, DynamicsTriggers,
    GENERATION_TIMING_ID, GenReason, GenRequest, GenerationTiming, GenerationTimingModel,
    MessageGenerator,
};
pub use j2735::bsm::{
    BasicSafetyMessage, BsmCoreData, BsmInput, PartIIContent, PartIIValue, VehicleSafetyExtensions,
};
pub use j2735::map::MapData;
pub use j2735::spat::Spat;
pub use j2735::{J2735_BSM_CODEC_ID, J2735_INFRA_CODEC_ID, J2735BsmCodec, J2735InfraCodec};
pub use size_model::{ContentProfile, J2735_SIZE_MODEL_ID, J2735SizeCodec, SizeRequest};

/// What `build.rs` generated, for the manifest and for anyone auditing the build.
///
/// A run's manifest pins the engine build; for a crate whose types come out of a code
/// generator, "the engine build" has to include *which ASN.1 modules were compiled and
/// which patches were applied*, or two builds that disagree about a message format would
/// look identical in the record.
pub mod provenance {
    /// The ASN.1 modules compiled into [`crate::asn1`], in the order `build.rs` adds them.
    pub const FACILITIES_MODULES: &[&str] = &[
        "ETSI-ITS-CDD (TS 102 894-2, Release 2, forge commit 615593c445d0)",
        "CAM-PDU-Descriptions (TS 103 900, Release 2, forge commit 649ada78da45)",
        "DENM-PDU-Description (TS 103 831, Release 2, forge commit 58472e2644a6)",
    ];

    /// The ASN.1 modules compiled into [`crate::sec_types`].
    pub const SECURITY_MODULES: &[&str] = &[
        "Ieee1609Dot2BaseTypes (forge commit 77e2c822a11b)",
        "Ieee1609Dot2 (forge commit 77e2c822a11b)",
        "Ieee1609Dot2CrlBaseTypes (forge commit 77e2c822a11b)",
        "Ieee1609Dot2Crl (forge commit 77e2c822a11b)",
        "EtsiTs103097ExtensionModule (TS 103 097, forge commit 6e01cab9f15c)",
        "EtsiTs103097Module (TS 103 097, forge commit 6e01cab9f15c)",
    ];

    /// Patches applied to the generated text (build decision D5).
    pub const PATCHES: &[&str] = &["0001-ieee1609dot2-endentitytype-default.patch"];

    /// The code generator, pinned in the workspace manifest.
    pub const GENERATOR: &str = "rasn-compiler 0.16 (RasnBackend) over rasn 0.28";

    /// Message formats this crate encodes **by hand**, and what each was validated against.
    ///
    /// A manifest that only listed the generated modules would imply that everything else
    /// was generated too. Build decision D2 leaves the J2735 messages to hand-written
    /// encoders, and a run's record has to say which ones exist and what each was checked
    /// against — together with the evidence, because "hand-written" and
    /// "hand-written and cross-validated against an independent implementation" are very
    /// different claims about the same bytes.
    pub const HAND_WRITTEN: &[&str] = &[
        "SAE J2735 2024-09 BasicSafetyMessage — Part I in full, the \
         VehicleSafetyExtensions Part II container and the MessageFrame wrapper \
         (crate::j2735::bsm); cross-validated against pycrate 0.8.1 compiled from the real \
         ASN.1, 235 vectors, byte-identical",
        "SAE J2735 2024-09 SPAT — timeStamp, IntersectionState (id, revision, status, \
         moy, timeStamp), MovementState, MovementEvent and TimeChangeDetails, plus the \
         MessageFrame wrapper (crate::j2735::spat); NOT cross-validated: no oracle run, the \
         ASN.1 is absent from this checkout",
        "SAE J2735 2024-09 MapData — msgIssueRevision, timeStamp, IntersectionGeometry \
         (id, revision, refPoint, laneWidth), GenericLane (attributes, maneuvers, node list, \
         connections), plus the MessageFrame wrapper (crate::j2735::map); NOT \
         cross-validated, and its extension markers are recalled rather than read — see \
         crate::j2735::map::assumptions",
    ];

    /// Message formats a **size model** stands in for, and why each is not encoded.
    ///
    /// The manifest needs this beside [`HAND_WRITTEN`] for one reason: a reader who sees
    /// only the generated modules and the hand-written list would reasonably assume
    /// everything else in the message set is encoded too. These are the ones that are not,
    /// and a size taken from them is a table lookup.
    pub const SIZE_MODELLED: &[&str] = &[
        "SAE J2735 PSM, SRM, SSM (codec/size-model/j2735) — the J2735 ASN.1 cannot be \
         code-generated (build decision D2). The VRU device does not use this row for its \
         PSM: it sends crate::j2735::psm's hand-written UPER (not oracle-validated)",
        "ETSI CPM (codec/size-model/etsi) — generatable from the published forge module, \
         but CPM-PDU-Descriptions.asn is not committed to third_party/asn1/etsi in this \
         checkout. The VAM row stays behind the generic codec seam; the VRU device sends \
         crate::vam's bytes, generated from the committed TS 103 300-3 module",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::model::Model;

    /// Build decision D9's scanning test, at the scope this crate can enforce: every float
    /// that reaches an artefact must sit on a declared grid.
    ///
    /// The artefacts this crate writes are model cards. Every message field it encodes is
    /// an *integer* by the time it reaches the wire (that is what an ASN.1 data element
    /// is), so the cards are where a stray float could hide — a threshold, a default, a
    /// tolerance. This walks every card's JSON and fails on any non-integer number that is
    /// off the 1e-6 grid.
    #[test]
    fn no_exported_float_is_off_its_quantisation_grid() {
        /// The finest grid any parameter in this crate declares: angles, at 1 µrad.
        const FINEST_QUANTUM: f64 = 1e-6;

        fn scan(value: &serde_json::Value, path: &str, offenders: &mut Vec<String>) {
            match value {
                serde_json::Value::Number(n) => {
                    if let Some(x) = n.as_f64()
                        && n.as_i64().is_none()
                        && n.as_u64().is_none()
                        && !v2xw_core::math::is_on_grid(x, FINEST_QUANTUM)
                    {
                        offenders.push(format!("{path} = {x}"));
                    }
                }
                serde_json::Value::Array(items) => {
                    for (i, item) in items.iter().enumerate() {
                        scan(item, &format!("{path}[{i}]"), offenders);
                    }
                }
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        scan(v, &format!("{path}.{k}"), offenders);
                    }
                }
                _ => {}
            }
        }

        let cards = [
            EtsiUperCodec::new().card().clone(),
            J2735BsmCodec::new().card().clone(),
            J2735InfraCodec::new().card().clone(),
            J2735SizeCodec::new().card().clone(),
            EtsiSizeCodec::new().card().clone(),
            CamGenerator::default().card().clone(),
            BsmGenerator::default().card().clone(),
        ];
        let mut offenders = Vec::new();
        for card in &cards {
            let json: serde_json::Value =
                serde_json::from_str(&card.to_json().expect("card serialises")).expect("json");
            scan(&json, &card.id, &mut offenders);
        }
        assert!(
            offenders.is_empty(),
            "floats off the {FINEST_QUANTUM} grid reached a model card: {offenders:?}"
        );
    }

    /// Every card this crate builds must pass the registry's own validation, or the models
    /// cannot be registered at all.
    #[test]
    fn every_card_validates() {
        for card in [
            EtsiUperCodec::new().card().clone(),
            J2735BsmCodec::new().card().clone(),
            J2735InfraCodec::new().card().clone(),
            J2735SizeCodec::new().card().clone(),
            EtsiSizeCodec::new().card().clone(),
            CamGenerator::default().card().clone(),
            BsmGenerator::default().card().clone(),
        ] {
            card.validate()
                .unwrap_or_else(|e| panic!("{} failed validation: {e}", card.id));
            card.check_api_version()
                .unwrap_or_else(|e| panic!("{}: {e}", card.id));
        }
    }

    /// The manifest's two prose lists must account for exactly the messages the evidence
    /// table says they do.
    ///
    /// Prose lists rot. This one is what a run's manifest prints, so a message that
    /// quietly moved tier — a size model replaced by an encoder, or the reverse — would
    /// leave the manifest describing the previous build. The counts are checked rather
    /// than the text, because the text is a sentence per message and the count is the part
    /// that cannot be right by accident.
    #[test]
    fn the_manifest_lists_account_for_every_message_they_claim() {
        use evidence::ByteExactness;

        let hand_written_codecs = [J2735_BSM_CODEC_ID, J2735_INFRA_CODEC_ID];
        let hand_written = EVIDENCE
            .iter()
            .filter(|e| e.codec.is_some_and(|id| hand_written_codecs.contains(&id)))
            .count();
        assert_eq!(
            hand_written,
            provenance::HAND_WRITTEN.len(),
            "{hand_written} messages are hand-encoded but the manifest lists {}",
            provenance::HAND_WRITTEN.len()
        );

        let mut modelled_codecs: Vec<&str> = EVIDENCE
            .iter()
            .filter(|e| e.exactness == ByteExactness::SizeModelled)
            .filter_map(|e| e.codec)
            .collect();
        modelled_codecs.sort_unstable();
        modelled_codecs.dedup();
        assert_eq!(
            modelled_codecs.len(),
            provenance::SIZE_MODELLED.len(),
            "{modelled_codecs:?} size-model the message set but the manifest lists {}",
            provenance::SIZE_MODELLED.len()
        );

        // And the unvalidated encoders must be named as unvalidated in the manifest, not
        // listed beside the BSM as though a pycrate run had covered them.
        let unvalidated = provenance::HAND_WRITTEN
            .iter()
            .filter(|line| line.contains("NOT cross-validated"))
            .count();
        assert_eq!(
            unvalidated,
            EVIDENCE
                .iter()
                .filter(|e| e.exactness == ByteExactness::RealUperUnvalidated)
                .count(),
            "the manifest must say which hand-written encoders are unvalidated: {:?}",
            provenance::HAND_WRITTEN
        );
    }

    /// The provenance list must actually match the patches on disk, or the manifest would
    /// record a build that never happened.
    #[test]
    fn the_recorded_patches_are_the_patches_on_disk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("patches");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("the patches directory exists")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".patch"))
            .collect();
        on_disk.sort();
        assert_eq!(on_disk, provenance::PATCHES);
    }
}
