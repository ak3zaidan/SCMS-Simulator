//! Revocation end to end, with the latency of every stage.
//!
//! One misbehaviour report becomes a vehicle that cannot sign, across five organisations
//! and four flows, and the decomposition of how long that took is a Phase 2 acceptance
//! criterion (05-protocols.md §8, 08-measurement-and-data.md §2.9). These tests walk the
//! whole path and assert on the decomposition rather than on the outcome, because "the
//! vehicle was revoked eventually" is not the measurement.
//!
//! Both distribution paths are exercised: the cellular uplink from the CRL Store, and the
//! 5.9 GHz broadcast from the roadside. They differ in exactly one modelled thing — the
//! link — which is what makes "how long until an RSU-only vehicle enforces it" a separate
//! number from "how long until a connected one does", and
//! [`the_two_distribution_paths_are_reported_separately`] prints both.

mod common;

use common::{DEVICE_A, DEVICE_B, DEVICE_C, deployment, provisioned};
use v2xw_core::time::Duration;
use v2xw_proto::net::Transport;
use v2xw_proto::pseudonym::ChangeReason;
use v2xw_proto::scms::run::{ScmsRun, crl_bytes};
use v2xw_proto::stage::{FlowRun, StageId};
use v2xw_proto::{CredentialService, PseudonymStrategy, RevocationLatency};

/// Certificates per i-period in these tests. Two is the minimum the resolution needs: the
/// Misbehaviour Authority correlates *two* reports about two different pseudonyms, which is
/// the whole point of asking the Linkage Authorities whether they belong to one device.
const JMAX: u32 = 2;

/// The four runs one revocation is spread over.
struct Revocation {
    report: FlowRun,
    resolution: FlowRun,
    issuance: FlowRun,
    distribution: FlowRun,
}

/// Provisions `DEVICE_A`, has `DEVICE_B` report two of its pseudonyms, resolves, issues a
/// CRL and delivers it to `to` over `path`.
fn revoke(run: &mut ScmsRun, to: v2xw_core::ids::NodeId, path: Transport) -> Revocation {
    provisioned(run, DEVICE_A, 0, 1, JMAX);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    let report = run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("the reports run");
    let (resolution, issuance) = run.investigate(0, 1, 0, JMAX).expect("two reports");
    run.run().expect("the investigation runs");
    let distribution = match path {
        Transport::V2xAir => {
            run.attach_rsu(to);
            run.broadcast_crl_to(to)
        }
        _ => run.distribute_crl(to),
    };
    run.run().expect("the distribution runs");
    Revocation {
        report,
        resolution,
        issuance,
        distribution,
    }
}

// -----------------------------------------------------------------------------------------
// The path
// -----------------------------------------------------------------------------------------

/// The whole path, through both Linkage Authorities, ending in a vehicle that cannot sign.
#[test]
fn a_report_becomes_a_vehicle_that_cannot_sign() {
    let mut run = deployment(2);
    let r = revoke(&mut run, DEVICE_A, Transport::CellularUu);

    // The Misbehaviour Authority resolved the device, and it took *both* LAs to do it:
    // either one alone answers a single bit and cannot name the device.
    let case = run.state.ma.case.as_ref().expect("a case was opened");
    assert!(case.resolved, "the two reports must resolve to one device");
    assert_eq!(
        case.same,
        [Some(true), Some(true)],
        "both Linkage Authorities must have answered"
    );
    assert_eq!(
        case.seeds.iter().filter(|s| s.is_some()).count(),
        2,
        "the CRL entry needs one seed from each LA"
    );

    // The passive half fired too: the RA will not issue to this enrolment certificate again.
    assert!(
        run.state.ra.blocklist.contains(&DEVICE_A),
        "the enrolment certificate must be blocklisted at the RA"
    );

    // And the active half reached the vehicle, which is enforcing it.
    let dev = &run.state.devices[&DEVICE_A];
    assert_eq!(
        dev.revoked_credentials().len(),
        JMAX as usize,
        "every certificate of the revoked period must match"
    );
    assert!(dev.silenced, "the device must have stopped transmitting");

    // The decomposition exists, is ordered, and every stage of 05-protocols §8's table
    // that this path passes through is present.
    let svc_stages: Vec<StageId> = [
        StageId::Detect,
        StageId::ReportSent,
        StageId::ReportReceived,
        StageId::Decision,
        StageId::Resolved,
        StageId::Issued,
        StageId::Published,
        StageId::Downloaded,
        StageId::Processed,
        StageId::Enforced,
    ]
    .into_iter()
    .collect();
    let log = &run.kernel.stages;
    let mut last = 0u64;
    for stage in svc_stages {
        let t = [r.report, r.resolution, r.issuance, r.distribution]
            .into_iter()
            .find_map(|fr| log.at(fr, stage))
            .unwrap_or_else(|| panic!("stage {stage} was never stamped"));
        assert!(
            t >= last,
            "stage {stage} at {t} is before the previous stage at {last}"
        );
        last = t;
    }
}

/// The CRL forces the pseudonym store to give up its active certificate, whatever the
/// scenario's change strategy says — including `silent`.
#[test]
fn a_revoked_pseudonym_is_dropped_even_under_the_silent_strategy() {
    let params = v2xw_proto::scms::params::ScmsParams::default().quick();
    let mut svc = CredentialService::new(params, 0)
        .expect("encodes")
        .with_batch(1, JMAX);
    svc.bootstrap(DEVICE_A, 0, PseudonymStrategy::Silent);
    svc.bootstrap(DEVICE_B, 0, PseudonymStrategy::Silent);
    svc.advance_to(1_000 * v2xw_core::time::NS_PER_S)
        .expect("runs");

    let installed = svc.now();
    let e = svc.rotate(DEVICE_A, installed).expect("start-up");
    assert_eq!(e.reason, Some(ChangeReason::Startup));
    assert!(svc.active(DEVICE_A, installed).is_some());

    // Report, resolve, issue, distribute — through the driver, as the engine would.
    let (lv0, lv1) = {
        let d = &svc.deployment().state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    svc.deployment_mut().submit_report(DEVICE_B, 0, lv0);
    svc.deployment_mut().submit_report(DEVICE_B, 0, lv1);
    svc.advance_to(2_000 * v2xw_core::time::NS_PER_S)
        .expect("runs");
    svc.deployment_mut()
        .investigate(0, 1, 0, JMAX)
        .expect("two reports");
    svc.advance_to(3_000 * v2xw_core::time::NS_PER_S)
        .expect("runs");
    svc.fetch_crl(DEVICE_A);
    svc.advance_to(4_000 * v2xw_core::time::NS_PER_S)
        .expect("runs");

    let now = svc.now();
    let e = svc
        .rotate(DEVICE_A, now)
        .expect("a revoked pseudonym forces a change");
    assert_eq!(
        e.reason,
        Some(ChangeReason::Revoked),
        "the reason must say the CRL did it, not that the certificate expired"
    );
    assert!(
        svc.active(DEVICE_A, now).is_none(),
        "with every pseudonym of the period revoked, the vehicle has nothing to sign with"
    );
    assert!(svc.installed(DEVICE_A, now).iter().all(|p| p.revoked));
}

// -----------------------------------------------------------------------------------------
// The latency decomposition
// -----------------------------------------------------------------------------------------

/// Both distribution paths, each decomposed, printed with `--nocapture`.
///
/// The assertions are the invariants the number must satisfy; the printed table is the
/// measurement. The RSU path is slower per byte — 6 Mbit/s against the cellular link's
/// 10 Mbit/s — and faster per hop, because a broadcast is one hop where a fetch is two.
#[test]
fn the_two_distribution_paths_are_reported_separately() {
    for (label, target, path) in [
        ("cellular", DEVICE_A, Transport::CellularUu),
        ("rsu", DEVICE_C, Transport::V2xAir),
    ] {
        let mut run = deployment(3);
        let r = revoke(&mut run, target, path);
        let svc_run = run;

        let svc = CredentialService::from_run(svc_run);
        let lat = RevocationLatency::assemble(
            svc.stages(),
            target,
            path,
            r.report,
            r.resolution,
            r.issuance,
            r.distribution,
        );

        assert!(
            lat.at(StageId::Detect).is_some(),
            "{label}: the path must start at a detection"
        );
        assert!(
            lat.at(StageId::Enforced).is_some(),
            "{label}: and end at enforcement"
        );
        let total = lat.total().expect("both ends present");
        assert!(
            total.as_nanos() > 0,
            "{label}: the whole path must take a positive amount of simulated time"
        );

        println!("\n=== revocation latency, {label} path ===");
        println!("  {:<22} {:>14}  {:>12}", "stage", "t (ns)", "delta");
        let mut prev = None;
        for (stage, t) in &lat.stages {
            let delta = prev.map_or(String::from("-"), |p: u64| {
                format!("{} us", (t - p) / 1_000)
            });
            println!("  {:<22} {t:>14}  {delta:>12}", stage.as_str());
            prev = Some(*t);
        }
        println!(
            "  total detect -> enforced: {} us  ({} ms)",
            total.as_nanos() / 1_000,
            total.as_nanos() / 1_000_000
        );
        // The RA's report shuffle dominates everything else by three orders of magnitude,
        // and it is a *policy* parameter rather than a cost: CAMP-EE SCMS-765 sets it to
        // "10,000 reports or one day", `ScmsParams::quick` shortens it to a minute so a
        // test can run through, and a deployment choosing five minutes would land
        // somewhere else again. So the number worth comparing between paths is the rest of
        // the path, reported separately rather than buried inside a total nobody can use.
        let after_shuffle = lat
            .between(StageId::Shuffled, StageId::Enforced)
            .expect("both present");
        println!(
            "  shuffle window (policy): {} us",
            lat.between(StageId::ReportSent, StageId::Shuffled)
                .expect("both present")
                .as_nanos()
                / 1_000
        );
        println!(
            "  shuffle -> enforced (cost): {} us  ({} ms)",
            after_shuffle.as_nanos() / 1_000,
            after_shuffle.as_nanos() / 1_000_000
        );
        assert!(
            after_shuffle.as_nanos() > 0,
            "{label}: the path after the shuffle must cost real time"
        );
        if let Some(b) = lat.crl_bytes {
            println!("  CRL delivered: {b} B over {}", path.as_str());
        }

        // The stages are non-decreasing, which is what makes the deltas meaningful.
        assert!(
            lat.stages.windows(2).all(|w| w[0].1 <= w[1].1),
            "{label}: {:?}",
            lat.stages
        );
        // And every delta is accounted for: the parts sum to the whole.
        let summed: u64 = lat.steps().iter().map(|(_, _, d)| d.as_nanos()).sum();
        assert_eq!(
            summed,
            Duration::between(
                lat.stages.first().expect("non-empty").1,
                lat.stages.last().expect("non-empty").1
            )
            .as_nanos(),
            "{label}: the stage deltas must sum to the span"
        );
    }
}

/// What a real CRL costs to deliver over each path, at the published size.
///
/// The test deployments above revoke one device, so their CRL is one entry. The number a
/// deployment needs is the full list: [BRECHT §VI-G] and 04-models.md §9.6 put a
/// 10,000-entry linked CRL at about 400 kB, and at that size the link is the whole story —
/// which is exactly why the two paths are modelled as links with bandwidths rather than as
/// one "CRL arrives" event.
#[test]
fn the_published_crl_size_dominates_the_delivery_time_on_both_paths() {
    let run = deployment(1);
    let sizes = &run.state.sizes;
    let p = &run.state.params;

    let one = crl_bytes(sizes, 1).bytes();
    let full = crl_bytes(sizes, 10_000).bytes();
    assert!(
        (390_000..=420_000).contains(&full),
        "a 10,000-entry linked CRL must be about 400 kB, got {full} B"
    );

    let cellular = v2xw_proto::Link {
        latency: p.uu_link_latency,
        bandwidth_bps: p.uu_link_bandwidth_bps,
        transport: Transport::CellularUu,
    };
    let air = v2xw_proto::Link {
        latency: p.v2x_air_latency,
        bandwidth_bps: p.v2x_air_bandwidth_bps,
        transport: Transport::V2xAir,
    };

    println!("\n=== CRL delivery time by path and size ===");
    for (label, bytes) in [("1 entry", one), ("10,000 entries", full)] {
        println!(
            "  {label:<16} {bytes:>7} B   cellular {:>8} us   5.9 GHz air {:>8} us",
            cellular.delay(bytes).as_nanos() / 1_000,
            air.delay(bytes).as_nanos() / 1_000
        );
    }

    // At one entry the fixed latency dominates and cellular is the slower path; at the
    // published size the bandwidth dominates and the air interface is.
    assert!(cellular.delay(one) > air.delay(one));
    assert!(air.delay(full) > cellular.delay(full));
}
