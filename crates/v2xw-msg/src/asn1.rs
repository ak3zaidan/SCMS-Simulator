//! The generated ETSI facilities bindings.
//!
//! `build.rs` runs `rasn-compiler` over the committed ETSI forge modules and writes
//! `$OUT_DIR/etsi_facilities.rs`; this module `include!`s it. Nothing here is hand-written,
//! and editing the generated file has no effect — it is rebuilt from the `.asn` sources.
//!
//! # What is in here
//!
//! | Rust module | ASN.1 module | Standard |
//! |---|---|---|
//! | [`facilities::etsi_its_cdd`] | `ETSI-ITS-CDD` | ETSI TS 102 894-2, Release 2 |
//! | [`facilities::cam_pdu_descriptions`] | `CAM-PDU-Descriptions` | ETSI TS 103 900, Release 2 |
//! | [`facilities::denm_pdu_description`] | `DENM-PDU-Description` | ETSI TS 103 831, Release 2 |
//!
//! Note the DENM module's ASN.1 name is `DENM-PDU-Description`, singular, although the
//! file is `DENM-PDU-Descriptions.asn`. That is upstream's spelling and the generated
//! module name follows it.
//!
//! # Naming
//!
//! `rasn-compiler` snake-cases ASN.1 module names and keeps type names as written, so
//! `CAM-PDU-Descriptions.CamParameters` becomes
//! `facilities::cam_pdu_descriptions::CamParameters`. Field names are snake-cased and
//! carry a `#[rasn(identifier = "…")]` with the original spelling, which is what keeps the
//! encoding faithful.
//!
//! # Why one crate holds these
//!
//! Every `include!` of the generated text creates *distinct* Rust types. Two crates that
//! each included it could not pass a `CAM` to one another. So the bindings live here once
//! and everyone else uses them from here — including `v2xw-sec`, which uses
//! [`crate::sec_types`] for the same reason.

/// The generated ETSI facilities modules.
///
/// `missing_docs` is allowed because the doc comments are whatever the ASN.1 comments say,
/// and several upstream types carry none; `clippy::all` is allowed because generated code
/// is not ours to restyle.
#[allow(missing_docs)]
#[allow(clippy::all)]
#[allow(clippy::pedantic)]
pub mod facilities {
    include!(concat!(env!("OUT_DIR"), "/etsi_facilities.rs"));
}

pub use facilities::cam_pdu_descriptions as cam_asn1;
pub use facilities::denm_pdu_description as denm_asn1;
pub use facilities::etsi_its_cdd as cdd;
