//! `v2xw-sec` — the security envelope, the cryptographic primitives, and the SCMS
//! scientific core.
//!
//! Four things, in increasing order of how badly a mistake here would corrupt a result:
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | IEEE 1609.2 `HashedId3/8/10` | [`hashedid`] | IEEE 1609.2a-2017 §6.3.25-6.3.26 |
//! | Primitive sizes, security levels, cost anchors | [`primitive`] | 04-models.md §9.4 |
//! | Real and modelled crypto backends, ECQV | [`crypto`] | 03-interfaces.md §6, SEC 4 §3.4-3.5 |
//! | The `SignedData` envelope and its verify plan | [`envelope`] | IEEE 1609.2 §5.3.1, TS 103 097 |
//! | Certificate construction | [`cert`] | 04-models.md §9.2 |
//! | Butterfly key expansion (CAMP SCP1) | [`butterfly`] | Brecht 2018 §V |
//! | Linkage values and linked CRLs (CAMP SCP2) | [`linkage`] | Brecht 2018 §V-B |
//! | P-256 arithmetic and the two reductions | [`ec`] | FIPS 186-4, SEC 1 |
//! | The AES-128 block permutation | [`aes128`] | FIPS 197 |
//!
//! # Where the generated ASN.1 types come from
//!
//! From `v2xw-msg`, never by including the generated file again. `v2xw_msg::sec_types` is
//! the single `include!` of the IEEE 1609.2 / TS 103 097 bindings, and that module's own
//! documentation explains why a second one would be a defect rather than a duplication:
//! `include!` is textual, so two crates that each included it would define two *unrelated*
//! sets of identically named types, and `Certificate` built here could not be put in a
//! message there.
//!
//! # The two invariants this crate is responsible for
//!
//! * **I-S1** — for any run, [`crypto::Real`] and [`crypto::Modeled`] produce identical
//!   verification outcomes and identical sizes. Not a hope: a modelled token is exactly
//!   the descriptor's `sig_bytes` long and a modelled public key is 33 bytes in the SEC 1
//!   compressed shape, so every ASN.1 alternative and therefore every encoded length is
//!   the same in both modes, and nothing in [`envelope`] branches on the mode.
//!   `tests/equivalence.rs` is the acceptance test.
//! * **I-S3** — a [`envelope::SecuredPdu`]'s size equals payload plus envelope overhead
//!   computed from the profile tables, and the real-encoder tier asserts *equality*, not
//!   a tolerance. `tests/overhead.rs` measures the overhead against 04-models.md §9.1's
//!   derivation, byte for byte.
//!
//! # The SCMS core is a port, and is held to the reference byte for byte
//!
//! [`butterfly`] and [`linkage`] reimplement `legacy/scms_sim_ref/scms_core/`. They are
//! not "equivalent to" it: `tests/legacy_vectors.rs` replays concrete vectors captured by
//! running the Python and asserts the Rust reproduces every byte. Where the legacy test
//! suite used a random seed, a fixed seed was substituted and the resulting values pinned.
//!
//! # Determinism
//!
//! Every number this crate produces is an integer or a byte string, so the
//! transcendental and quantisation rules of ADR 0004 bite in exactly one place: the cost
//! tables of [`primitive`], whose microsecond values are quantised to the nanosecond grid
//! at declaration ([`primitive::COST_US_QUANTUM`]) and converted to an integer
//! [`v2xw_core::time::Duration`] when charged. No standard-library transcendental is
//! called anywhere in the crate; nothing here needs one.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod aes128;
pub mod butterfly;
pub mod cert;
pub mod crypto;
pub mod ec;
pub mod envelope;
pub mod error;
pub mod hashedid;
pub mod linkage;
pub mod primitive;
pub mod testctx;

pub use crypto::{
    CryptoBackend, CryptoBackendInfo, KeyHandle, KeyId, MODE_INDEPENDENT_PRIMITIVES, MODELED_ID,
    Modeled, PubHandle, REAL_ID, Real, SigToken, runs_in_both_modes,
};
pub use ec::Point;
pub use envelope::{
    CrlStore, ETSI_TS_103_097_ID, Envelope, EnvelopeProfile, GenerationLocation, HeaderInfoSpec,
    IEEE_1609_2_ID, P2pcdRequest, ParsedSecured, ParsedSigner, PeerCertCache, PlanOutcome,
    PrimitiveOp, RejectReason, SecuredPdu, SecurityEnvelope, SecurityEnvelopeInfo, SignerHandle,
    SignerIdChoice, SignerIdPolicy, TrustStore, VerifyPlan,
};
pub use error::{Result, SecError};
pub use hashedid::{certificate_digest, hashed_id3, hashed_id8, hashed_id10};
pub use linkage::{CrlLinkageEntry, DeviceLinkageContext, LaId, LinkageSeed, LinkageValue};
pub use primitive::{
    CostRow, CostTable, OpCost, Primitive, PrimitiveCatalogue, PrimitiveDescriptor,
    PrimitiveFamily, PrimitiveId, PrimitiveOpKind, SizeSpec,
};

/// Registers every model this crate provides with a [`Registry`].
///
/// One call rather than a list the caller has to keep in step: the envelopes, one model
/// per primitive in the standard catalogue, and both crypto backends. A model without a
/// card cannot be registered (03-interfaces.md §12), so this is also the assertion that
/// every card in the crate validates.
///
/// [`Registry`]: v2xw_core::registry::Registry
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
    wall: v2xw_core::time::WallClock,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    use std::sync::Arc;
    use v2xw_core::model::ModelHandle;

    let mut refs = Vec::new();
    let handles: Vec<ModelHandle> = vec![
        Arc::new(Envelope::ieee1609(wall)),
        Arc::new(Envelope::etsi(wall)),
        Arc::new(Real::new()),
        Arc::new(Modeled::new()),
    ];
    for h in handles {
        refs.push(registry.register_model(h)?);
    }
    for p in primitive::standard_primitive_models() {
        refs.push(registry.register_model(Arc::new(p))?);
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::registry::Registry;
    use v2xw_core::time::WallClock;

    /// Every model in the crate registers, which means every card validates and no two
    /// models share an id.
    #[test]
    fn every_model_in_the_crate_registers() {
        let mut registry = Registry::new();
        let refs = register_all(&mut registry, WallClock::default()).expect("all register");
        assert_eq!(refs.len(), registry.len());
        assert_eq!(
            refs.len(),
            4 + PrimitiveCatalogue::standard().len(),
            "two envelopes, two backends, and one model per primitive"
        );
        for id in [
            IEEE_1609_2_ID,
            ETSI_TS_103_097_ID,
            REAL_ID,
            MODELED_ID,
            PrimitiveId::ECDSA_P256_SHA256.as_str(),
            PrimitiveId::ML_DSA_44.as_str(),
        ] {
            assert!(registry.contains(id), "{id} is not registered");
        }
        // Registering twice must fail rather than shadow: two models with one id would
        // make the manifest's model pin ambiguous.
        assert!(register_all(&mut registry, WallClock::default()).is_err());
    }

    /// No model in the crate leaves a parameter uncalibrated without a plan (rule R1).
    #[test]
    fn no_model_has_an_uncalibrated_parameter_without_a_plan() {
        let mut registry = Registry::new();
        register_all(&mut registry, WallClock::default()).expect("all register");
        for (r, p) in registry.todo_calibrate_report() {
            assert!(
                p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()),
                "{r:?} / {}: todo-calibrate with no plan",
                p.name
            );
        }
    }
}
