//! Linkage resolution identifies the right device from two reports — and no other.

mod common;

use common::{DEVICE_A, DEVICE_B, DEVICE_C, deployment, provisioned};
use v2xw_proto::scms::msg::LaIndex;
use v2xw_proto::stage::StageId;

#[test]
fn two_reports_about_one_device_resolve_to_that_device_and_blocklist_only_it() {
    let mut run = deployment(3);
    provisioned(&mut run, DEVICE_A, 0, 2, 2);
    provisioned(&mut run, DEVICE_B, 0, 2, 2);
    provisioned(&mut run, DEVICE_C, 0, 2, 2);

    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_B];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_A, 0, lv0);
    run.submit_report(DEVICE_C, 0, lv1);
    run.run().expect("reports run");
    assert_eq!(run.state.ma.reports.len(), 2);

    let (res, _crl) = run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("investigation runs");

    let case = run.state.ma.case.as_ref().expect("a case");
    assert!(case.resolved, "both Linkage Authorities must agree");
    assert_eq!(
        run.state.ra.blocklist.iter().copied().collect::<Vec<_>>(),
        vec![DEVICE_B],
        "exactly the reported device, and no other"
    );
    assert!(run.kernel.stages.at(res, StageId::Resolved).is_some());
    assert_eq!(run.state.ma.decisions.len(), 1);
}

#[test]
fn two_reports_about_two_devices_do_not_resolve() {
    let mut run = deployment(3);
    provisioned(&mut run, DEVICE_B, 0, 1, 2);
    provisioned(&mut run, DEVICE_C, 0, 1, 2);
    let lv_b = run.state.devices[&DEVICE_B].credentials[&(0, 0)].lv;
    let lv_c = run.state.devices[&DEVICE_C].credentials[&(0, 0)].lv;
    run.submit_report(DEVICE_A, 0, lv_b);
    run.submit_report(DEVICE_A, 0, lv_c);
    run.run().expect("reports run");

    let (res, _crl) = run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("investigation runs");

    let case = run.state.ma.case.as_ref().expect("a case");
    assert_eq!(case.same, [Some(false), Some(false)]);
    assert!(!case.resolved);
    assert!(
        run.state.ra.blocklist.is_empty(),
        "an unresolved case must revoke nobody"
    );
    assert!(
        run.kernel.stages.at(res, StageId::Resolved).is_none(),
        "and must not stamp `resolved`"
    );
    assert!(run.state.crl_store.entries.is_empty());
}

#[test]
fn certificates_from_two_different_provisioning_requests_still_resolve_to_one_device() {
    // The case the Pseudonym Certificate Authority cannot settle on its own: the two
    // certificates carry different chain identifiers, so only the Linkage Authorities can
    // say they are one device. Both must be asked, and both must agree.
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_B, 0, 1, 2);
    run.topup(DEVICE_B, 1, 2);
    run.run().expect("top-up runs");

    let (lv_first, lv_second) = {
        let d = &run.state.devices[&DEVICE_B];
        (d.credentials[&(0, 0)].lv, d.credentials[&(1, 0)].lv)
    };
    run.submit_report(DEVICE_A, 0, lv_first);
    run.submit_report(DEVICE_A, 1, lv_second);
    run.run().expect("reports run");

    let (a, b) = {
        let issued = &run.state.pca.issued;
        (
            issued[&(0, *lv_first.as_bytes())],
            issued[&(1, *lv_second.as_bytes())],
        )
    };
    assert_ne!(
        a.lci1, b.lci1,
        "two provisioning requests must get two chains, or the PCA could link them itself"
    );
    assert_ne!(a.request_hash, b.request_hash);

    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("investigation runs");
    let case = run.state.ma.case.as_ref().expect("a case");
    assert_eq!(case.same, [Some(true), Some(true)]);
    assert!(case.resolved);
    assert_eq!(
        run.state.ra.blocklist.iter().copied().collect::<Vec<_>>(),
        vec![DEVICE_B]
    );
}

#[test]
fn a_linkage_authority_answers_one_bit_and_the_ma_needs_both() {
    // Either LA alone must be unable to settle a case: the MA waits for two answers and
    // requires both to be true (05-protocols §3.2, [BRECHT §VI-C]).
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_B, 0, 1, 2);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_B];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_A, 0, lv0);
    run.submit_report(DEVICE_A, 0, lv1);
    run.run().expect("runs");
    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");

    let same_device_steps = run
        .kernel
        .steps
        .iter()
        .filter(|s| s.step == "same-device-request")
        .count();
    assert_eq!(same_device_steps, 2, "one query to each Linkage Authority");
    let responses = run
        .kernel
        .steps
        .iter()
        .filter(|s| s.step == "same-device-response")
        .count();
    assert_eq!(responses, 2);
    // And each LA holds one of the two chains for this device, never both.
    let chains_1 = run.state.la[LaIndex::One.idx()].chains.len();
    let chains_2 = run.state.la[LaIndex::Two.idx()].chains.len();
    assert_eq!((chains_1, chains_2), (1, 1));
}

#[test]
fn a_linkage_value_the_pca_never_issued_resolves_to_nothing() {
    use v2xw_sec::linkage::LinkageValue;
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_B, 0, 1, 2);
    let real = run.state.devices[&DEVICE_B].credentials[&(0, 0)].lv;
    run.submit_report(DEVICE_A, 0, real);
    run.submit_report(DEVICE_A, 0, LinkageValue::new([0xab; 9]));
    run.run().expect("runs");
    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");
    let case = run.state.ma.case.as_ref().expect("a case");
    assert_eq!(
        case.lookups.len(),
        1,
        "the PCA answers `not found` for the forgery"
    );
    assert!(!case.resolved);
    assert!(run.state.ra.blocklist.is_empty());
}
