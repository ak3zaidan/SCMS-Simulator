//! Backward privacy: revocation is forward-only.
//!
//! The property 05-protocols calls out specifically. Publishing a revoked device's linkage
//! seeds for period `i` must not make its certificates from *before* `i` linkable — "a
//! vehicle is trackable only after revocation". Two things have to hold, and both are
//! tested here through the real flows rather than by calling the primitive directly:
//!
//! 1. the Linkage Authorities release `ls_x(i_rev)` and never `ls_x(0)`, so nothing
//!    downstream even holds the material an earlier period would need;
//! 2. the CRL entry refuses to match an earlier period outright, so a verifier that
//!    somehow held an earlier seed still would not answer.
//!
//! The injected-fault test at the end proves the check can fail: it hands the CRL an entry
//! built from the *initial* seed instead of the revocation-period seed, and the earlier
//! certificates become linkable. That is what the protocol must not do, and the test is
//! red when the protocol does it.

mod common;

use common::{DEVICE_A, DEVICE_B, deployment, provisioned};
use v2xw_sec::linkage::{self, CrlLinkageEntry};

const I_REV: u32 = 3;
const PERIODS: u32 = 6;
const JMAX: u32 = 2;

/// Provisions a device across six i-periods, revokes it from period 3 and distributes the
/// CRL through the real flows.
fn revoked_from_period_three() -> v2xw_proto::scms::run::ScmsRun {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, PERIODS, JMAX);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("reports run");
    run.investigate(0, 1, I_REV, JMAX).expect("two reports");
    run.run().expect("investigation runs");
    run.distribute_crl(DEVICE_A);
    run.run().expect("distribution runs");
    run
}

#[test]
fn the_linkage_authorities_release_the_revocation_period_seed_and_not_the_initial_one() {
    let run = revoked_from_period_three();
    let entry = *run.crl_entry().expect("one entry on the published list");
    assert_eq!(entry.i, I_REV);

    for (la, published) in [
        (&run.state.la[0], entry.ls1_i),
        (&run.state.la[1], entry.ls2_i),
    ] {
        let (_, ls0) = la.chains.iter().next().expect("one chain");
        assert_ne!(
            published.as_bytes(),
            ls0.as_bytes(),
            "the initial seed must never be published"
        );
        assert_eq!(
            published.as_bytes(),
            linkage::linkage_seed_at(la.la_id, *ls0, I_REV).as_bytes(),
            "what is published is ls(i_rev), exactly"
        );
    }
}

#[test]
fn certificates_from_before_the_revocation_period_are_not_linkable() {
    let run = revoked_from_period_three();
    let dev = &run.state.devices[&DEVICE_A];
    assert_eq!(dev.crl.len(), 1, "the device processed the published list");

    for i in 0..I_REV {
        for j in 0..JMAX {
            assert!(
                !dev.is_revoked(i, j),
                "certificate ({i},{j}) predates the revocation and must stay unlinkable"
            );
        }
    }
}

#[test]
fn certificates_from_the_revocation_period_forward_are_linkable() {
    let run = revoked_from_period_three();
    let dev = &run.state.devices[&DEVICE_A];
    for i in I_REV..PERIODS {
        for j in 0..JMAX {
            assert!(
                dev.is_revoked(i, j),
                "certificate ({i},{j}) is at or after the revocation and must match"
            );
        }
    }
    let revoked = dev.revoked_credentials();
    assert_eq!(
        revoked.len() as u32,
        (PERIODS - I_REV) * JMAX,
        "exactly the periods from the revocation forward"
    );
    assert!(
        dev.silenced,
        "a device that finds itself listed stops transmitting [CAMP-EE §2.2.10.2 step 8.4]"
    );
}

#[test]
fn an_unrevoked_device_matches_nothing_on_the_same_list() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, PERIODS, JMAX);
    provisioned(&mut run, DEVICE_B, 0, PERIODS, JMAX);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("runs");
    run.investigate(0, 1, I_REV, JMAX).expect("two reports");
    run.run().expect("runs");
    run.distribute_crl(DEVICE_B);
    run.run().expect("runs");

    let other = &run.state.devices[&DEVICE_B];
    assert_eq!(other.crl.len(), 1);
    assert!(
        other.revoked_credentials().is_empty(),
        "the list must not match a device it does not name"
    );
    assert!(!other.silenced);
}

#[test]
fn the_forward_only_check_can_fail_when_the_initial_seed_is_published() {
    // The injected fault. If a Linkage Authority released ls(0) instead of ls(i_rev), the
    // entry would reach backwards and the earlier certificates would become linkable. This
    // test builds exactly that entry and asserts it *does* link them — so the three tests
    // above are measuring the protocol's choice and not a property of the hash function.
    let run = revoked_from_period_three();
    let dev = &run.state.devices[&DEVICE_A];

    let leaky = CrlLinkageEntry {
        i: 0,
        la_id1: run.state.la[0].la_id,
        la_id2: run.state.la[1].la_id,
        ls1_i: *run.state.la[0].chains.values().next().expect("one chain"),
        ls2_i: *run.state.la[1].chains.values().next().expect("one chain"),
        jmax: JMAX,
        max_forward: linkage::DEFAULT_MAX_FORWARD_PERIODS,
    };

    for i in 0..I_REV {
        for j in 0..JMAX {
            let lv = dev.credentials[&(i, j)].lv;
            assert!(
                leaky.matches(i, j, lv),
                "the fault must be detectable: an entry built from ls(0) links ({i},{j})"
            );
            // And the entry the protocol actually published does not.
            let real = run.crl_entry().expect("one entry");
            assert!(!real.matches(i, j, lv));
        }
    }
}
