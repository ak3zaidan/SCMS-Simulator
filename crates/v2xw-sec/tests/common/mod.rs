//! A small SCMS the integration tests share: a root authority, a pseudonym CA and an
//! end entity, each with a real certificate the other tests sign and verify against.
//!
//! Built through the [`CryptoBackend`] trait alone, so the same code constructs the
//! hierarchy under [`Real`](v2xw_sec::Real) and under
//! [`Modeled`](v2xw_sec::Modeled). That is not incidental: the I-S1 equivalence test
//! depends on the two modes producing *the same shaped* hierarchy, and a helper that
//! reached for a real curve point would have made the modelled run impossible to set up
//! rather than merely different.
//!
//! `dead_code` is allowed below because this module is compiled once per integration-test
//! binary and no single one of them uses every helper: `overhead.rs` never touches the CRL
//! stores, `legacy_vectors.rs` never builds a PKI at all. Gating each item on which test
//! file happens to need it would be churn that serves no reader.

#![allow(dead_code)]

use std::sync::Arc;

use v2xw_core::ids::NodeId;
use v2xw_msg::sec_types::Certificate;
use v2xw_sec::cert::{self, CertSpec, HolderId};
use v2xw_sec::crypto::{CryptoBackend, CryptoBackendInfo, KeyHandle};
use v2xw_sec::envelope::SignerHandle;
use v2xw_sec::hashedid::certificate_digest;
use v2xw_sec::linkage::{DeviceLinkageContext, LaId, LinkageSeed};
use v2xw_sec::primitive::PrimitiveId;
use v2xw_sec::testctx::TestCtx;
use v2xw_sec::{PeerCertCache, TrustStore};

/// The Unix timestamp of 2026-01-01T00:00:00Z, for the scenario wall clock.
pub const T0_UNIX: i64 = 1_767_225_600;

/// Seconds from the IEEE 1609.2 epoch (2004-01-01) to `T0_UNIX`.
///
/// Derived rather than written out, because a hand-computed epoch offset is exactly the
/// kind of constant that is wrong by one day and produces certificates that are valid,
/// verifiable and stamped in the wrong year.
pub const T0_1609_SECONDS: u32 = (T0_UNIX - v2xw_core::time::IEEE1609_EPOCH_UNIX_S) as u32;

/// The PSID the tests sign under: 0x20, the value 04-models.md §9.1 uses in its
/// envelope-overhead derivation.
pub const PSID_CAM: u64 = 0x20;

/// A PSID at or above 128, which needs three COER bytes instead of two.
pub const PSID_WIDE: u64 = 0x8000;

/// One node's credentials.
pub struct Entity {
    /// The node.
    pub node: NodeId,
    /// Its signing key.
    pub key: KeyHandle,
    /// Its certificate.
    pub certificate: Arc<Certificate>,
    /// The signer handle the envelope takes.
    pub signer: SignerHandle,
}

/// A root authority, a pseudonym CA under it, and some end entities under that.
pub struct Pki {
    /// The self-signed root, which is the only trust anchor.
    pub root: Arc<Certificate>,
    /// The root's digest.
    pub root_digest: v2xw_msg::sec_types::HashedId8,
    /// The pseudonym CA, issued by the root.
    pub pca: Arc<Certificate>,
    /// The PCA's digest.
    pub pca_digest: v2xw_msg::sec_types::HashedId8,
    /// The end entities, in node order.
    pub entities: Vec<Entity>,
}

impl Pki {
    /// A trust store holding the root only.
    pub fn trust_store(&self) -> TrustStore {
        let mut t = TrustStore::new();
        t.insert(self.root.clone()).expect("the root encodes");
        t
    }

    /// A peer cache that already holds the PCA (verified) and every end-entity
    /// certificate (verified), which is the steady state after P2PCD has settled.
    pub fn warm_cache(&self) -> PeerCertCache {
        let mut c = PeerCertCache::new();
        c.insert(self.pca.clone(), true).expect("encodes");
        for e in &self.entities {
            c.insert(e.certificate.clone(), true).expect("encodes");
        }
        c
    }
}

/// The linkage context the pseudonym certificates' `linkageData` comes from.
pub fn device_linkage(device: u8) -> DeviceLinkageContext {
    DeviceLinkageContext::new(
        LaId(0x0001),
        LaId(0x0002),
        LinkageSeed::new([device; 16]),
        LinkageSeed::new([device.wrapping_add(0x80); 16]),
    )
}

/// Builds the hierarchy with `backend`, giving `count` end entities.
///
/// The root signs its own certificate with a zero signature, because a trust anchor's
/// signature is never checked — it is trusted for being in the store. Everything below it
/// is signed for real: the PCA by the root, each end entity by the PCA, each over the
/// IEEE 1609.2 §6.4.3 digest.
pub fn build_pki<B>(ctx: &mut TestCtx, backend: &mut B, count: u32) -> Pki
where
    B: CryptoBackend<TestCtx> + CryptoBackendInfo,
{
    let p = PrimitiveId::ECDSA_P256_SHA256;

    // --- the root ----------------------------------------------------------------------
    let root_key = backend
        .keygen(ctx, p, NodeId::new(900))
        .expect("root keygen");
    let root_material = backend
        .public_material(&backend.public_of(&root_key).expect("pub"))
        .expect("material");
    let root = Arc::new(
        cert::trust_anchor(
            &CertSpec::authority(T0_1609_SECONDS, PSID_CAM),
            &root_material,
        )
        .expect("root builds"),
    );
    let root_coer = cert::encode(&root).expect("root encodes");
    let root_digest = certificate_digest(&root).expect("root digest");

    // --- the pseudonym CA, issued by the root -----------------------------------------
    let pca_key = backend
        .keygen(ctx, p, NodeId::new(901))
        .expect("pca keygen");
    let pca_material = backend
        .public_material(&backend.public_of(&pca_key).expect("pub"))
        .expect("material");
    let mut pca_spec = CertSpec::authority(T0_1609_SECONDS, PSID_CAM);
    pca_spec.issuer = Some(root_digest.clone());
    let pca = Arc::new(
        cert::issue_explicit(&pca_spec, &pca_material, &root_coer, |digest| {
            let token = backend.sign_prehashed(ctx, &root_key, digest)?;
            token.to_ieee1609_signature()
        })
        .expect("pca issues"),
    );
    let pca_coer = cert::encode(&pca).expect("pca encodes");
    let pca_digest = certificate_digest(&pca).expect("pca digest");

    // --- the end entities, issued by the PCA ------------------------------------------
    let mut entities = Vec::new();
    for n in 0..count {
        let node = NodeId::new(n);
        let key = backend.keygen(ctx, p, node).expect("ee keygen");
        let material = backend
            .public_material(&backend.public_of(&key).expect("pub"))
            .expect("material");
        let dev = device_linkage((n + 1) as u8);
        let spec = CertSpec::pseudonym(
            pca_digest.clone(),
            HolderId::Linkage {
                i_cert: 7,
                linkage_value: dev.linkage_value_for(7, u32::from(n as u16)),
            },
            T0_1609_SECONDS,
            PSID_CAM,
        );
        let certificate = Arc::new(
            cert::issue_explicit(&spec, &material, &pca_coer, |digest| {
                let token = backend.sign_prehashed(ctx, &pca_key, digest)?;
                token.to_ieee1609_signature()
            })
            .expect("ee issues"),
        );
        let signer = SignerHandle::new(node, key, certificate.clone()).expect("signer");
        entities.push(Entity {
            node,
            key,
            certificate,
            signer,
        });
    }

    Pki {
        root,
        root_digest,
        pca,
        pca_digest,
        entities,
    }
}
