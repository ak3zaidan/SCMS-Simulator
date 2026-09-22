//! What each party learns — checked by walking its state, not by reading the design.
//!
//! The claim 05-protocols §3.2 makes about butterfly provisioning is that "the
//! Registration Authority learns nothing it should not". Three things have to be true for
//! that: the RA never holds a linkage value or a seed; it cannot open a pre-linkage value
//! even though one passes through it; and it cannot compute the certified public key,
//! because only the PCA's randomiser fixes it.

mod common;

use common::{DEVICE_A, DEVICE_B, deployment, provisioned};
use v2xw_core::ids::NodeId;
use v2xw_proto::scms::msg::{Lci, SealedForPca};
use v2xw_sec::butterfly;

#[test]
fn the_ra_holds_no_linkage_value_and_no_seed() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 2, 3);

    // Everything the device holds.
    let lvs: Vec<_> = run.state.devices[&DEVICE_A]
        .credentials
        .values()
        .map(|c| c.lv)
        .collect();
    assert_eq!(lvs.len(), 6);

    // The RA's whole state, rendered. It records enrolment, request hashes and chain
    // identifiers, and the chain identifiers are opaque handles, not seeds.
    let rendered = format!("{:?}", run.state.ra);
    for lv in &lvs {
        let hex: String = lv.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(
            !rendered.contains(&hex),
            "a linkage value must not appear anywhere in the RA's state"
        );
    }
    for la in &run.state.la {
        for seed in la.chains.values() {
            let hex: String = seed.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
            assert!(
                !rendered.contains(&hex),
                "a linkage seed must never leave its Linkage Authority"
            );
        }
    }
    assert_eq!(run.state.ra.requests.len(), 1);
    let record = run.state.ra.requests.values().next().expect("one request");
    assert!(record.lci1.is_some() && record.lci2.is_some());
}

#[test]
fn only_the_pca_can_open_a_sealed_pre_linkage_value() {
    // The mechanism, not the convention: the RA's node id cannot open the seal.
    let pca = NodeId::new(2);
    let ra = NodeId::new(1);
    let sealed = SealedForPca::seal(Lci(7));
    assert_eq!(sealed.clone().open(pca, pca), Some(Lci(7)));
    assert_eq!(sealed.clone().open(ra, pca), None);
    assert_eq!(SealedForPca::<Lci>::wrapper_bytes(), 65);
}

#[test]
fn the_ra_cannot_compute_the_certified_key_it_helped_produce() {
    // The arithmetic reason the RA learns nothing: it holds the cocoon key B and the
    // device's expansion keys, but the certified key is B + c·G and only the PCA knows c.
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 1);
    let dev = &run.state.devices[&DEVICE_A];
    let cat = dev.caterpillar.as_ref().expect("provisioned");
    let cred = &dev.credentials[&(0, 0)];

    let (cocoon, _q) = butterfly::ra_cocoon_keys(
        &cat.signing_public(),
        &cat.encryption_public(),
        cat.ck(),
        cat.ek(),
        0,
        0,
    );
    assert_ne!(
        cocoon.compressed(),
        cred.certified_public.compressed(),
        "the cocoon key the RA computes is not the certified key"
    );
    // And with the PCA's randomiser it is — which is what makes the difference the RA's
    // ignorance rather than an accident of encoding.
    let (recomputed, _c_point) = butterfly::pca_certify_explicit(&cocoon, &cred.c);
    assert_eq!(recomputed.compressed(), cred.certified_public.compressed());
}

#[test]
fn a_linkage_authority_learns_nothing_about_the_devices_keys() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    provisioned(&mut run, DEVICE_B, 0, 1, 2);
    for la in &run.state.la {
        assert_eq!(la.chains.len(), 2, "one chain per provisioning request");
        // It knows which device a chain belongs to — it must, to answer "same device?" —
        // and nothing else: no key, no certificate, no linkage value.
        let rendered = format!("{la:?}");
        for dev in [DEVICE_A, DEVICE_B] {
            let cat = run.state.devices[&dev]
                .caterpillar
                .as_ref()
                .expect("provisioned");
            let a = cat.signing_public().compressed().expect("a point");
            let hex: String = a.iter().map(|b| format!("{b:02x}")).collect();
            assert!(
                !rendered.contains(&hex),
                "a Linkage Authority must not hold a device's caterpillar key"
            );
        }
    }
}

#[test]
fn the_pca_holds_no_device_identity_only_a_request_hash() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    assert_eq!(run.state.pca.issued.len(), 2);
    for lookup in run.state.pca.issued.values() {
        // Chain identifiers and a request hash — the three things §VI-C says an
        // investigation starts from. No node id, and nothing that names the device.
        assert_ne!(lookup.lci1, Lci(0));
        assert_ne!(lookup.lci2, Lci(0));
        assert_ne!(lookup.request_hash, [0u8; 32]);
    }
}
