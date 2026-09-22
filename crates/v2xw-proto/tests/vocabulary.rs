//! The recorded vocabulary, and the determinism of the engine seam.
//!
//! Two properties that are invisible until they break.
//!
//! **One spelling per stage.** 05-protocols.md §8 fixes the stage names that "revocation
//! latency by stage" is defined over, and [`StageId::as_str`] returns them. A recording
//! that serialised a *second* spelling of the same stage would still verify, still look
//! right, and silently answer a different question from the one the document defines — the
//! query would find `report-sent` where the metric is defined over `report_sent`. This
//! crate had exactly that defect: the enum carried `rename_all = "kebab-case"` while
//! `as_str` returned snake case.
//!
//! **A reproducible driver.** The engine seam is new machinery around the flows: a horizon,
//! a record cursor, an injection clamp. None of it may make a run depend on how it was
//! stepped.

mod common;

use common::{DEVICE_A, DEVICE_B};
use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};
use v2xw_proto::pseudonym::{CERTCHG_INTERVAL, ChangeReason};
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::stage::{FlowId, StageId};
use v2xw_proto::{CredentialService, PseudonymStrategy};

/// Every stage 05-protocols §8 and §3.2 name, so the table below cannot fall behind the
/// enum by omission: the crate's own flow declarations must be a subset of it.
const ALL_STAGES: [StageId; 23] = [
    StageId::Detect,
    StageId::ReportSent,
    StageId::ReportReceived,
    StageId::Decision,
    StageId::Resolved,
    StageId::Issued,
    StageId::Published,
    StageId::FirstRsuBroadcast,
    StageId::Downloaded,
    StageId::Processed,
    StageId::Enforced,
    StageId::ResidualHarm,
    StageId::Blocklisted,
    StageId::LastValidCredentialExpiry,
    StageId::Requested,
    StageId::ProxyForwarded,
    StageId::Acknowledged,
    StageId::Expanded,
    StageId::PreLinkageReady,
    StageId::Shuffled,
    StageId::Certified,
    StageId::BatchReady,
    StageId::Installed,
];

/// A recorded stage name is the name the design set spells, for every stage.
#[test]
fn stage_names_serialise_as_the_design_set_spells_them() {
    for stage in ALL_STAGES {
        let json = serde_json::to_string(&stage).expect("serialises");
        let serialised = json.trim_matches('"');
        assert_eq!(
            serialised,
            stage.as_str(),
            "stage {stage:?} is recorded as `{serialised}` but 05-protocols §8 spells it \
             `{}`; a metric defined over one name cannot find the other",
            stage.as_str()
        );
        // And it round-trips, so a reader can parse what a writer wrote.
        let back: StageId = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, stage);
    }
    // The two the document spells out explicitly, as a spot check on the whole rule.
    assert_eq!(StageId::ReportSent.as_str(), "report_sent");
    assert_eq!(StageId::FirstRsuBroadcast.as_str(), "first_rsu_broadcast");
}

/// The injected fault: the check above must be able to go red.
///
/// A same-shaped enum under the renaming this crate used to carry, so the test proves it
/// is comparing a serialised name against `as_str` and not comparing `as_str` to itself.
#[test]
fn the_stage_name_check_can_fail() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "kebab-case")]
    enum Wrong {
        ReportSent,
    }
    let json = serde_json::to_string(&Wrong::ReportSent).expect("serialises");
    assert_eq!(
        json.trim_matches('"'),
        "report-sent",
        "the renaming this crate used to carry"
    );
    assert_ne!(
        json.trim_matches('"'),
        StageId::ReportSent.as_str(),
        "and it is not the name the design set spells, so the check above can fail"
    );
}

/// Every stage a flow declares is in the vocabulary, and every flow's name round-trips too.
#[test]
fn every_declared_stage_is_in_the_vocabulary() {
    for flow in v2xw_proto::scms::FLOWS {
        for stage in flow.stages {
            assert!(
                ALL_STAGES.contains(stage),
                "flow {} declares {stage:?}, which this test's table does not list",
                flow.id
            );
        }
        let json = serde_json::to_string(&flow.id).expect("serialises");
        assert_eq!(json.trim_matches('"'), flow.id.as_str());
    }
}

// -----------------------------------------------------------------------------------------
// Determinism of the engine seam
// -----------------------------------------------------------------------------------------

/// The step size the engine drives with must not change what the backend produces.
///
/// Two runs of the same bootstrap, one stepped every 100 ms and one every 3 s, must produce
/// the same stage log to the nanosecond. If the horizon leaked into a service time or a
/// link delay, this is where it would show.
#[test]
fn the_engine_step_size_does_not_change_the_result() {
    fn run(step: Duration) -> Vec<(StageId, SimTime)> {
        let t0: SimTime = 1_500 * NS_PER_MS;
        let mut svc = CredentialService::new(ScmsParams::default().quick(), t0).expect("encodes");
        let b = svc.bootstrap(DEVICE_A, t0, PseudonymStrategy::default());
        let mut now = t0;
        while now <= t0 + 300 * NS_PER_S {
            svc.advance_to(now).expect("runs");
            svc.drain();
            now = step.after(now);
        }
        svc.decomposition(b.provisioning)
    }
    let fine = run(Duration::from_millis(100));
    let coarse = run(Duration::from_secs(3));
    assert!(!fine.is_empty());
    assert_eq!(
        fine, coarse,
        "the backend's stage instants must not depend on how the engine stepped it"
    );
}

/// The same master seed produces the same linkage values, and a different one does not.
///
/// Every secret in the provisioning flow — the device's caterpillar, each Linkage
/// Authority's initial seed, the PCA's per-certificate randomiser — comes from an
/// `RngRegistry` stream keyed by `(RngDomain::Crypto, EntityRef::Node(..))`, so the
/// linkage values a run produces are a function of the seed and nothing else.
#[test]
fn the_seed_determines_the_credentials_and_nothing_else_does() {
    fn lvs(seed: u64) -> Vec<[u8; 9]> {
        let params = ScmsParams {
            master_seed: seed,
            ..ScmsParams::default().quick()
        };
        let mut svc = CredentialService::new(params, 0)
            .expect("encodes")
            .with_batch(1, 4);
        svc.bootstrap(DEVICE_A, 0, PseudonymStrategy::default());
        svc.advance_to(1_000 * NS_PER_S).expect("runs");
        svc.installed(DEVICE_A, svc.now())
            .into_iter()
            .map(|p| p.linkage_value)
            .collect()
    }
    let a = lvs(0xC0FF_EE5E);
    assert_eq!(a.len(), 4);
    assert_eq!(a, lvs(0xC0FF_EE5E), "the same seed must replay exactly");
    assert_ne!(
        a,
        lvs(0xC0FF_EE5F),
        "and a different seed must give an independent replication"
    );
}

/// Rotation is a function of the instants it is asked about, not of how often it is asked.
#[test]
fn rotation_does_not_depend_on_how_often_it_is_polled() {
    fn changes(poll: Duration) -> Vec<(u32, u32, ChangeReason)> {
        let mut svc = CredentialService::new(ScmsParams::default().quick(), 0)
            .expect("encodes")
            .with_batch(1, 8);
        svc.bootstrap(
            DEVICE_A,
            0,
            PseudonymStrategy::Time {
                period: CERTCHG_INTERVAL,
            },
        );
        svc.advance_to(1_000 * NS_PER_S).expect("runs");
        let start = svc.now();
        let mut out = Vec::new();
        let mut now = start;
        // A whole number of periods, so the two polling rates see the same boundaries.
        while now <= start + 1_800 * NS_PER_S {
            if let Some(e) = svc.rotate(DEVICE_A, now) {
                out.push((e.i_period, e.j_index, e.reason.expect("a reason")));
            }
            now = poll.after(now);
        }
        out
    }
    let fine = changes(Duration::from_millis(100));
    let coarse = changes(Duration::from_secs(1));
    assert_eq!(fine.len(), 7, "start-up plus six 300 s periods in 1,800 s");
    assert_eq!(fine, coarse);
}

/// Two devices bootstrapped in the same step get different pseudonyms.
///
/// The streams are keyed by node, so a second vehicle is not a copy of the first — which
/// would make every privacy measurement in the simulator meaningless.
#[test]
fn two_devices_in_one_step_get_different_pseudonyms() {
    let mut svc = CredentialService::new(ScmsParams::default().quick(), 0)
        .expect("encodes")
        .with_batch(1, 4);
    svc.bootstrap(DEVICE_A, 0, PseudonymStrategy::default());
    svc.bootstrap(DEVICE_B, 0, PseudonymStrategy::default());
    svc.advance_to(1_000 * NS_PER_S).expect("runs");
    let now = svc.now();
    let a: Vec<[u8; 9]> = svc
        .installed(DEVICE_A, now)
        .into_iter()
        .map(|p| p.linkage_value)
        .collect();
    let b: Vec<[u8; 9]> = svc
        .installed(DEVICE_B, now)
        .into_iter()
        .map(|p| p.linkage_value)
        .collect();
    assert_eq!(a.len(), 4);
    assert_eq!(b.len(), 4);
    assert!(
        a.iter().all(|x| !b.contains(x)),
        "no linkage value may be shared between two devices"
    );
}

/// A device that spawns late is provisioned late, and its records say so.
#[test]
fn a_late_spawn_is_provisioned_late() {
    let t0: SimTime = 0;
    let mut svc = CredentialService::new(ScmsParams::default().quick(), t0).expect("encodes");
    let early = svc.bootstrap(DEVICE_A, t0, PseudonymStrategy::default());
    let late_at = 30 * NS_PER_S;

    // Drive up to the late spawn, then add the second vehicle, exactly as a demand model
    // arriving mid-run would.
    let mut now = t0;
    while now < late_at {
        svc.advance_to(now).expect("runs");
        now = Duration::from_millis(100).after(now);
    }
    let late = svc.bootstrap(NodeId::new(1_001), late_at, PseudonymStrategy::default());
    while now <= 600 * NS_PER_S {
        svc.advance_to(now).expect("runs");
        now = Duration::from_millis(100).after(now);
    }

    let a = svc.provisioning_cost(early.provisioning).expect("complete");
    let b = svc.provisioning_cost(late.provisioning).expect("complete");
    assert!(
        b.requested >= late_at,
        "the late vehicle cannot have asked before it existed: {} < {late_at}",
        b.requested
    );
    assert!(
        b.requested > a.requested,
        "and it asked after the early one: {} vs {}",
        b.requested,
        a.requested
    );
    // Both are in the same i-period, because a week is longer than the run.
    assert_eq!(
        svc.decomposition(early.provisioning)
            .iter()
            .filter(|(s, _)| *s == StageId::Installed)
            .count(),
        1
    );
    assert_eq!(
        svc.deployment().state.devices[&DEVICE_A]
            .credentials
            .keys()
            .map(|&(i, _)| i)
            .max(),
        Some(0)
    );
    assert!(
        svc.stages().runs_of(FlowId::Provisioning).len() == 2,
        "two vehicles, two provisioning runs"
    );
}
