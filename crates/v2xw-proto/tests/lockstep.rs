//! Driving the deployment from an engine running it in lockstep with its own clock: the
//! pre-run pool, a report whose access leg the engine carried, the single-certificate
//! revocation, and the CRL Generator's cadence.

mod common;

use common::{DEVICE_A, DEVICE_B, deployment};
use v2xw_core::time::{Duration, NS_PER_S};
use v2xw_proto::net::Transport;
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::scms::run::ScmsRun;
use v2xw_proto::stage::StageId;

/// A pre-run pool exists with the whole backend state behind it, and the run's own clock,
/// queues and logs are untouched — a vehicle on the road did its provisioning last week.
#[test]
fn a_preloaded_pool_leaves_the_run_clock_untouched() {
    let mut run = ScmsRun::new(ScmsParams::default()).expect("encodes");
    run.preload(DEVICE_A, 0, 2, 3).expect("preloads");
    let dev = &run.state.devices[&DEVICE_A];
    assert_eq!(
        dev.credentials.len(),
        6,
        "two periods of three certificates"
    );
    // The LAs and the PCA hold what a later investigation needs.
    assert!(!run.state.pca.issued.is_empty());
    assert!(run.state.la.iter().all(|la| !la.chains.is_empty()));
    // With the cited one-day request shuffle a pool still arrives: the shuffle is in the
    // past, and a pre-run pool must not be held up by it.
    assert_eq!(run.kernel.now(), 0, "the run's clock moved");
    assert!(
        run.kernel.stages.stamps().is_empty(),
        "the run's stage log is not empty"
    );
    assert!(
        run.kernel.steps.is_empty(),
        "the run's wire log is not empty"
    );
    assert_eq!(
        run.state.params.shuffle_window,
        ScmsParams::default().shuffle_window,
        "the cited window must be restored after the preload"
    );
}

/// One report about one certificate is enough to revoke once the authority has decided,
/// and both Linkage Authorities must release a seed for an entry to exist.
#[test]
fn a_decided_certificate_is_revoked_through_both_linkage_authorities() {
    let mut run = deployment(2);
    run.preload(DEVICE_A, 0, 1, 2).expect("preloads");
    let lv = run.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
    // The engine carried the access leg: detected at 1 s, sent at 1.01 s, at the proxy at
    // 1.05 s over the cellular uplink.
    let report_run = run.report_at_proxy(
        DEVICE_B,
        0,
        lv,
        NS_PER_S,
        NS_PER_S + 10_000_000,
        NS_PER_S + 50_000_000,
        Transport::CellularUu,
        1_200,
    );
    run.run_until(200 * NS_PER_S).expect("runs");
    let log = &run.kernel.stages;
    assert_eq!(log.at(report_run, StageId::Detect), Some(NS_PER_S));
    let received = log
        .at(report_run, StageId::ReportReceived)
        .expect("the report reached the authority after the RA's shuffle");
    assert!(
        received >= 60 * NS_PER_S,
        "the quick shuffle window is a minute"
    );
    // The access leg is in the wire log with the transport the engine used.
    assert!(
        run.kernel.steps.iter().any(|s| s.run == report_run
            && s.transport == Transport::CellularUu
            && s.bytes == 1_200)
    );

    let (resolution, issuance) = run.revoke(0, 0, 2).expect("a report is held");
    assert!(run.case_open());
    assert!(
        run.revoke(0, 0, 2).is_none(),
        "cases are carried out one at a time"
    );
    run.run_until(400 * NS_PER_S).expect("runs");
    let case = run.state.ma.case.as_ref().expect("the case");
    assert!(case.done && case.resolved);
    assert_eq!(case.seeds.iter().filter(|s| s.is_some()).count(), 2);
    assert!(run.state.ra.blocklist.contains(&DEVICE_A));
    let log = &run.kernel.stages;
    for (stage, flow) in [
        (StageId::Decision, resolution),
        (StageId::Resolved, resolution),
        (StageId::Blocklisted, resolution),
        (StageId::Issued, issuance),
        (StageId::Published, issuance),
    ] {
        assert!(log.at(flow, stage).is_some(), "{stage} was not stamped");
    }
    assert_eq!(run.state.crl_store.entries.len(), 1);

    // The counterexample: an LA that does not know the chain releases nothing, and no entry
    // is issued — one authority cannot revoke on the other's behalf.
    let mut broken = deployment(2);
    broken.preload(DEVICE_A, 0, 1, 2).expect("preloads");
    let lv = broken.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
    broken.report_at_proxy(
        DEVICE_B,
        0,
        lv,
        NS_PER_S,
        NS_PER_S,
        NS_PER_S,
        Transport::CellularUu,
        1_200,
    );
    broken.run_until(200 * NS_PER_S).expect("runs");
    broken.state.la[1].chains.clear();
    broken.revoke(0, 0, 2).expect("a report is held");
    broken.run_until(400 * NS_PER_S).expect("runs");
    assert!(
        broken.state.crl_store.entries.is_empty(),
        "one LA's seed revoked a device"
    );
}

/// With a cadence, the CRL Generator holds an entry until the next boundary of the
/// calendar; on decision it publishes at once.
#[test]
fn the_crl_is_published_on_its_cadence() {
    let publish = |cadence_s: u64, on_cadence: bool| -> (u64, u64) {
        let mut params = ScmsParams::default().quick();
        params.crl_cadence = Duration::from_secs(cadence_s);
        params.publish_on_cadence = on_cadence;
        let mut run = ScmsRun::new(params).expect("encodes");
        run.add_device(DEVICE_A);
        run.add_device(DEVICE_B);
        run.preload(DEVICE_A, 0, 1, 2).expect("preloads");
        let lv = run.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
        run.report_at_proxy(DEVICE_B, 0, lv, 0, 0, 0, Transport::CellularUu, 1_200);
        run.run_until(100 * NS_PER_S).expect("runs");
        let (_, issuance) = run.revoke(0, 0, 2).expect("held");
        run.run_until(1_000 * NS_PER_S).expect("runs");
        let log = &run.kernel.stages;
        (
            log.at(issuance, StageId::Issued).expect("issued"),
            log.at(issuance, StageId::Published).expect("published"),
        )
    };
    let cadence = 300 * NS_PER_S;
    let (issued, published) = publish(300, true);
    // The next boundary after issuance, plus the Generator's signature and the backend
    // link to the Store (milliseconds).
    let boundary = (issued / cadence + 1) * cadence;
    assert!(
        published >= boundary && published - boundary < NS_PER_S,
        "published at {published} ns, not at the 300 s boundary {boundary} ns after \
         issuance at {issued} ns"
    );
    // The fault the rule exists against: without the cadence the entry goes out at once,
    // so the check above is not satisfied by accident.
    let (issued, published) = publish(300, false);
    assert!(
        published - issued < NS_PER_S,
        "on decision, publication is immediate"
    );
    assert!(published < (issued / cadence + 1) * cadence);
}
