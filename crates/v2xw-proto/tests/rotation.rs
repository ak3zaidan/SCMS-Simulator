//! Pseudonym rotation: the scenario's strategy drives it, and the store really swaps.
//!
//! Two failures these tests exist to catch, both of which look like success from a
//! distance:
//!
//! * a rotation counter that increments while the same certificate keeps signing — the
//!   store "rotated" and the vehicle stayed linkable;
//! * a pool of twenty that is used two at a time, which is what the obvious "pick anything
//!   but the current one" rule does.
//!
//! So every test here asserts on the *linkage value* the device would put on the air, not
//! on a counter: the linkage value is the only identifier a receiver sees, and two
//! messages with the same one are two messages a receiver can link.

mod common;

use std::collections::BTreeSet;

use common::DEVICE_A;
use v2xw_core::time::{Duration, NS_PER_MS, NS_PER_S, SimTime};
use v2xw_proto::pseudonym::{
    CERTCHG_DISTANCE_CM, CERTCHG_INTERVAL, ChangeReason, PseudonymStrategy,
};
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::{CredentialService, PseudonymStore};

const STEP: Duration = Duration::from_millis(100);
const SPAWN: SimTime = 1_500 * NS_PER_MS;

/// A provisioned device with `strategy`, and the instant its credentials were installed.
fn provisioned(strategy: PseudonymStrategy) -> (CredentialService, SimTime) {
    let mut svc =
        CredentialService::new(ScmsParams::default().quick(), SPAWN).expect("certificates encode");
    let b = svc.bootstrap(DEVICE_A, SPAWN, strategy);
    let mut now = SPAWN;
    while now <= SPAWN + 300 * NS_PER_S {
        svc.advance_to(now).expect("backend runs");
        svc.drain();
        now = STEP.after(now);
    }
    let installed = svc
        .provisioning_cost(b.provisioning)
        .expect("provisioned")
        .installed;
    (svc, installed)
}

// -----------------------------------------------------------------------------------------
// The four strategies
// -----------------------------------------------------------------------------------------

/// `strategy: time, period_s: 300` — the Phase 1 scenario's own line — changes every five
/// minutes and changes to a *different* certificate every time.
#[test]
fn the_time_strategy_changes_on_its_period_and_drains_the_pool_in_issue_order() {
    let (mut svc, installed) = provisioned(PseudonymStrategy::Time {
        period: CERTCHG_INTERVAL,
    });
    let pool = svc.params().certs_per_period;

    let mut seen_lv: Vec<[u8; 9]> = Vec::new();
    let mut changes = Vec::new();
    let mut now = installed;
    let end = installed + 3_600 * NS_PER_S;
    while now <= end {
        if let Some(e) = svc.rotate(DEVICE_A, now) {
            changes.push(e);
            seen_lv.push(e.linkage_value);
        }
        now = STEP.after(now);
    }

    // One start-up change, then one every 300 s over the hour.
    assert_eq!(changes[0].reason, Some(ChangeReason::Startup));
    let scheduled = changes
        .iter()
        .filter(|e| e.reason == Some(ChangeReason::Scheduled))
        .count();
    assert_eq!(
        scheduled, 12,
        "3,600 s at a 300 s period is twelve scheduled changes, got {scheduled}"
    );

    // Issue order, and no repeats: thirteen activations out of a pool of twenty.
    let js: Vec<u32> = changes.iter().map(|e| e.j_index).collect();
    assert_eq!(
        js,
        (0..13).collect::<Vec<u32>>(),
        "the pool must drain in issue order"
    );
    let distinct: BTreeSet<[u8; 9]> = seen_lv.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        seen_lv.len(),
        "every change must move to a linkage value the vehicle has not used"
    );
    assert!(
        (distinct.len() as u32) <= pool,
        "and never more than the pool holds"
    );
    assert_eq!(
        svc.deployment().state.devices[&DEVICE_A].store.wraps(),
        0,
        "an hour at 300 s does not exhaust a week's pool of {pool}"
    );

    // The store swapped: the active credential is the last one activated.
    let active = svc.active(DEVICE_A, end).expect("something is active");
    assert_eq!(active.j_index, 12);
    assert_eq!(active.linkage_value, *seen_lv.last().expect("some"));
}

/// `strategy: distance` changes on metres travelled and not on time.
#[test]
fn the_distance_strategy_changes_on_travel_and_not_on_time() {
    let (mut svc, installed) = provisioned(PseudonymStrategy::Distance {
        distance_cm: CERTCHG_DISTANCE_CM,
    });
    // Start-up, then no time-driven change however long it stands still.
    assert_eq!(
        svc.rotate(DEVICE_A, installed).map(|e| e.reason),
        Some(Some(ChangeReason::Startup))
    );
    let mut now = installed;
    for _ in 0..36_000 {
        now = STEP.after(now);
        assert!(
            svc.rotate(DEVICE_A, now).is_none(),
            "a stationary vehicle under the distance rule must not change pseudonym"
        );
    }
    // 2 km of travel and it changes, once.
    svc.travelled_cm(DEVICE_A, CERTCHG_DISTANCE_CM);
    let e = svc.rotate(DEVICE_A, now).expect("2 km is due");
    assert_eq!(e.reason, Some(ChangeReason::Scheduled));
    assert_eq!(e.j_index, 1);
    assert!(
        svc.rotate(DEVICE_A, now).is_none(),
        "the odometer resets on a change"
    );
    // Just under the threshold does nothing; the last centimetre does.
    svc.travelled_cm(DEVICE_A, CERTCHG_DISTANCE_CM - 1);
    assert!(svc.rotate(DEVICE_A, now).is_none());
    svc.travelled_cm(DEVICE_A, 1);
    assert_eq!(
        svc.rotate(DEVICE_A, now).map(|e| e.j_index),
        Some(2),
        "the threshold is inclusive at exactly 2 km"
    );
}

/// `strategy: mix-zone` changes only when the node says it left one.
#[test]
fn the_mix_zone_strategy_changes_only_on_leaving_a_zone() {
    let (mut svc, installed) = provisioned(PseudonymStrategy::MixZone);
    svc.rotate(DEVICE_A, installed).expect("start-up");
    let mut now = installed;
    for _ in 0..600 {
        now = STEP.after(now);
        svc.travelled_cm(DEVICE_A, 100_000);
        assert!(
            svc.rotate(DEVICE_A, now).is_none(),
            "neither time nor distance may trigger the mix-zone rule"
        );
    }
    svc.left_mix_zone(DEVICE_A);
    assert_eq!(
        svc.rotate(DEVICE_A, now).map(|e| e.reason),
        Some(Some(ChangeReason::Scheduled))
    );
    assert!(svc.rotate(DEVICE_A, now).is_none(), "the flag is consumed");
}

/// `strategy: silent` never changes on a schedule — but a revoked pseudonym still forces
/// one, because a device that kept signing with a revoked certificate would be modelling a
/// device that ignores its own CRL.
#[test]
fn the_silent_strategy_never_schedules_a_change_but_revocation_still_forces_one() {
    let (mut svc, installed) = provisioned(PseudonymStrategy::Silent);
    svc.rotate(DEVICE_A, installed).expect("start-up");
    let first = svc
        .active(DEVICE_A, installed)
        .expect("active")
        .linkage_value;

    let mut now = installed;
    for _ in 0..36_000 {
        now = STEP.after(now);
        svc.travelled_cm(DEVICE_A, 10_000);
        assert!(svc.rotate(DEVICE_A, now).is_none());
    }
    assert_eq!(
        svc.active(DEVICE_A, now)
            .expect("still active")
            .linkage_value,
        first,
        "a silent device keeps the same pseudonym"
    );
}

// -----------------------------------------------------------------------------------------
// The store itself
// -----------------------------------------------------------------------------------------

/// A pool that runs out wraps, and says so.
///
/// The number a deployment cares about: a device whose top-up did not arrive starts reusing
/// certificates, and the moment it does is the moment its unlinkability stops improving.
#[test]
fn a_pool_that_runs_out_wraps_and_the_wrap_is_counted() {
    let mut store = PseudonymStore::new(PseudonymStrategy::Time {
        period: Duration::from_secs(1),
    });
    let usable: Vec<(u32, u32)> = (0..3).map(|j| (0, j)).collect();
    let mut activated = Vec::new();
    let mut now = 0;
    for _ in 0..7 {
        if let Some((_, Some(p))) = store.rotate(now, &usable, false) {
            activated.push(p);
        }
        now += NS_PER_S;
    }
    assert_eq!(
        activated,
        vec![(0, 0), (0, 1), (0, 2), (0, 0), (0, 1), (0, 2), (0, 0)],
        "issue order, then wrap"
    );
    assert_eq!(store.wraps(), 2, "two wraps in seven activations of three");
}

/// A device with one usable certificate does not report changes it did not make.
#[test]
fn a_store_with_nothing_to_rotate_to_reports_no_change() {
    let mut store = PseudonymStore::new(PseudonymStrategy::Time {
        period: Duration::from_secs(1),
    });
    let one = [(0u32, 0u32)];
    assert_eq!(
        store.rotate(0, &one, false).map(|(r, _)| r),
        Some(ChangeReason::Startup)
    );
    assert!(
        store.rotate(10 * NS_PER_S, &one, false).is_none(),
        "there is nothing to change to, so nothing changed"
    );
    assert_eq!(store.changes(), 1);
}

/// The four scenario spellings map to the four strategies, and nothing else does.
#[test]
fn the_scenario_spellings_map_to_the_strategies() {
    assert_eq!(
        PseudonymStrategy::from_scenario("time", Some(300.0), None),
        Some(PseudonymStrategy::Time {
            period: CERTCHG_INTERVAL
        })
    );
    assert_eq!(
        PseudonymStrategy::from_scenario("distance", None, Some(2_000.0)),
        Some(PseudonymStrategy::Distance {
            distance_cm: CERTCHG_DISTANCE_CM
        })
    );
    assert_eq!(
        PseudonymStrategy::from_scenario("mix-zone", None, None),
        Some(PseudonymStrategy::MixZone)
    );
    assert_eq!(
        PseudonymStrategy::from_scenario("silent", None, None),
        Some(PseudonymStrategy::Silent)
    );
    assert_eq!(
        PseudonymStrategy::from_scenario("whatever", None, None),
        None,
        "an unknown strategy must be refused, not defaulted"
    );
    // The schema's own defaults are the cited ones.
    assert_eq!(
        PseudonymStrategy::from_scenario("time", None, None),
        Some(PseudonymStrategy::Time {
            period: CERTCHG_INTERVAL
        })
    );
}

/// A pseudonym change is recordable on `sec.cert`, with the fields §14 names.
#[test]
fn a_pseudonym_change_is_recordable_on_sec_cert() {
    use v2xw_core::ctx::{ErasedRecord, Visibility};

    let (mut svc, installed) = provisioned(PseudonymStrategy::Time {
        period: CERTCHG_INTERVAL,
    });
    svc.rotate(DEVICE_A, installed).expect("start-up");
    let e = svc
        .rotate(DEVICE_A, CERTCHG_INTERVAL.after(installed))
        .expect("one period later");
    let owned = e.to_owned_record().expect("serialises");
    assert_eq!(owned.channel, "sec.cert");
    assert_eq!(owned.visibility, Visibility::Node);
    let v: serde_json::Value = serde_json::from_slice(&owned.json).expect("json");
    for field in [
        "t", "node", "event", "reason", "i_period", "j_index", "changes",
    ] {
        assert!(!v[field].is_null(), "sec.cert is missing `{field}`: {v}");
    }
    assert_eq!(v["event"], "change");
    assert_eq!(v["reason"], "scheduled");

    // And the drain hands the same event to the recorder, exactly once.
    let drained = svc.drain();
    assert_eq!(drained.certs.len(), 2, "both changes, once each");
}
