//! Butterfly provisioning yields certificates the device can actually use — and the
//! Registration Authority cannot link them to it.

mod common;

use common::{DEVICE_A, deployment, provisioned};
use v2xw_proto::stage::{FlowId, StageId};

#[test]
fn every_provisioned_credential_is_usable_by_the_device() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 3, 4);
    let dev = &run.state.devices[&DEVICE_A];
    assert_eq!(dev.credentials.len(), 12, "3 i-periods × 4 indices");
    for &(i, j) in dev.credentials.keys() {
        assert!(
            dev.credential_is_usable(i, j),
            "b'({i},{j}) = a + f1(ck,(i,j)) + c must match the certified public key"
        );
    }
}

#[test]
fn a_credential_from_another_index_is_not_usable_for_this_one() {
    // The negative half: the identity is checked, not assumed. Swapping two credentials'
    // randomisers must break the match, or the check above would pass for anything.
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    let dev = run.state.devices.get_mut(&DEVICE_A).expect("device");
    let c0 = dev.credentials[&(0, 0)].c;
    let c1 = dev.credentials[&(0, 1)].c;
    assert_ne!(c0, c1, "the PCA draws a fresh randomiser per certificate");
    dev.credentials.get_mut(&(0, 0)).expect("cred").c = c1;
    assert!(
        !dev.credential_is_usable(0, 0),
        "the wrong randomiser must not derive the certified key"
    );
}

#[test]
fn the_pca_issued_exactly_the_requested_certificates() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 5, 2, 3);
    assert_eq!(run.state.pca.issued_count, 6);
    let dev = &run.state.devices[&DEVICE_A];
    for i in 5..7 {
        for j in 0..3 {
            assert!(dev.credentials.contains_key(&(i, j)), "({i},{j}) missing");
        }
    }
}

#[test]
fn provisioning_costs_real_queueing_time_and_real_bytes() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 20);
    let flow_run = run.kernel.stages.runs_of(FlowId::Provisioning)[0];
    let requested = run
        .kernel
        .stages
        .at(flow_run, StageId::Requested)
        .expect("requested");
    let installed = run
        .kernel
        .stages
        .at(flow_run, StageId::Installed)
        .expect("installed");
    assert!(installed > requested, "provisioning takes simulated time");
    assert!(
        run.kernel.bytes_of_flow(FlowId::Provisioning) > 0,
        "and it puts bytes on links"
    );
    // Every entity on the path actually served requests.
    for node in [
        run.state.nodes.lop,
        run.state.nodes.ra,
        run.state.nodes.la1,
        run.state.nodes.la2,
        run.state.nodes.pca,
    ] {
        let q = run.kernel.queue(node).expect("hosted");
        assert!(q.served() > 0, "{node} served nothing");
    }
    // And the PCA was charged for the per-certificate work.
    let pca_signs = run
        .kernel
        .ops
        .get(&(run.state.nodes.pca, "primitive/ecdsa-p256-sha256", "sign"))
        .copied()
        .unwrap_or(0);
    assert_eq!(pca_signs, 20, "one signature per issued certificate");
}

#[test]
fn a_blocklisted_device_is_refused_a_top_up() {
    // The passive half of revocation, measured where it bites: the RA stops issuing.
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    run.state.ra.blocklist.insert(DEVICE_A);
    let before = run.state.pca.issued_count;
    run.topup(DEVICE_A, 1, 2);
    run.run().expect("runs");
    assert_eq!(run.state.ra.refused, 1);
    assert_eq!(
        run.state.pca.issued_count, before,
        "no certificate may be issued to a blocklisted enrolment certificate"
    );
}
