//! The safety applications of 04-models.md §11, and the surrogate measures they emit.
//!
//! # Faults injected to prove these checks can fail
//!
//! 1. `time_to_collision_s` made to return `gap / closing.abs()` — a separating pair then
//!    gets a finite time to collision and `an_opening_pair_never_warns` fails.
//! 2. The corridor test in `rear_end_surrogates` removed —
//!    `a_vehicle_in_the_next_lane_is_not_a_rear_end_conflict` fails.
//! 3. The `pet <= pet_conflict_s` gate in `Ima::evaluate` removed —
//!    `two_paths_that_cross_at_different_times_are_not_a_conflict` fails, because a
//!    vehicle that has long gone produces an intersection warning.
//! 4. The `estimate_window` check in `Eebl::track` removed —
//!    `a_deceleration_estimated_over_too_long_a_window_is_refused` fails.
//! 5. `relevance_from_ttc` made constant — `the_relevance_scores_reach_the_on_demand_policy`
//!    fails, because a distant sender then scores the same as an imminent one.

use std::collections::BTreeMap;

use v2xw_core::belief::{FixQuality, PositionEstimate};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::rng::RngRegistry;
use v2xw_core::time::{NS_PER_MS, SimTime};
use v2xw_node::ctx::NodeRuntimeCtx;
use v2xw_node::safety::{
    Eebl, EeblParams, Fcw, Ima, SafetyApp, SafetyAppSet, Severity, TTC_CONFLICT_THRESHOLD_S,
    WarningKind, digest_key, relevance_from_ttc,
};
use v2xw_node::stores::{Neighbor, NeighborTable, VerificationState, pseudo_signer};

const EGO: NodeId = NodeId::new(1);
const PEER: NodeId = NodeId::new(9);

fn belief(pos: Vec3, speed: f64, heading: f64) -> PositionEstimate {
    let mut p = PositionEstimate::no_fix(0);
    p.pos = pos;
    p.heading_rad = heading;
    p.vel = Vec3::new(
        speed * v2xw_core::math::cos(heading),
        speed * v2xw_core::math::sin(heading),
        0.0,
    );
    p.fix = FixQuality::ThreeD;
    p
}

fn neighbour(j: u32, pos: Vec3, speed: f64, heading: f64, claimed_at: SimTime) -> Neighbor {
    Neighbor {
        signer: pseudo_signer(PEER, j),
        claimed_pos: pos,
        claimed_speed_mps: speed,
        claimed_heading_rad: heading,
        claimed_generation_time: claimed_at,
        last_heard: claimed_at,
        messages: 1,
        state: VerificationState::Verified,
    }
}

fn table(entries: Vec<Neighbor>) -> NeighborTable {
    let mut t = NeighborTable::new(32);
    for e in entries {
        t.observe(e);
    }
    t
}

/// The threshold this crate's applications use is the one `v2xw-metrics` uses for the
/// ground-truth metric.
///
/// The constant is duplicated on purpose — `v2xw-node` does not depend on `v2xw-metrics`
/// and must not — so it is pinned here. Two thresholds for one quantity would make the
/// belief-side and the truth-side numbers incomparable, which is the whole point of
/// computing both.
#[test]
fn the_conflict_threshold_is_the_one_the_metrics_crate_uses() {
    // FHWA-HRT-08-051 (SSAM), the one VERIFIED surrogate-safety threshold in
    // 04-models.md §11.
    assert_eq!(TTC_CONFLICT_THRESHOLD_S, 1.5);
}

/// A vehicle 30 m ahead doing 5 m/s while the ego does 20 m/s: closing at 15 m/s, so the
/// time to collision is 2 s and the FCW's 1.5 s threshold does not fire. Close the gap to
/// 20 m and it does, at a computed 1.333 s.
#[test]
fn a_closing_vehicle_ahead_warns_at_the_computed_ttc() {
    let rng = RngRegistry::new(11);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Fcw::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);

    let far = table(vec![neighbour(0, Vec3::new(30.0, 0.0, 0.0), 5.0, 0.0, 0)]);
    assert!(
        app.on_neighbors(&mut ctx, EGO, 0, &far, &ego).is_empty(),
        "a 2 s time to collision is outside the threshold"
    );

    let near = table(vec![neighbour(0, Vec3::new(20.0, 0.0, 0.0), 5.0, 0.0, 0)]);
    let out = app.on_neighbors(&mut ctx, EGO, NS_PER_MS, &near, &ego);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, WarningKind::Issue);
    // 20 / 15 = 1.3333…, quantised to 1e-3.
    assert_eq!(out[0].surrogates.ttc_s, 1.333);
    // The required deceleration is 15² / (2·20) = 5.625 m/s².
    assert_eq!(out[0].surrogates.required_decel_mps2, 5.625);
    assert_eq!(out[0].surrogates.closing_mps, 15.0);
    assert!(
        out[0].surrogates.pet_s.is_nan(),
        "a rear-end pair has no PET"
    );
    assert_eq!(out[0].severity, Severity::Warning);
}

/// A pair that is separating has no time to collision at all, so no warning is possible —
/// not a large one, not a negative one.
#[test]
fn an_opening_pair_never_warns() {
    let rng = RngRegistry::new(12);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Fcw::default();
    let ego = belief(Vec3::ZERO, 5.0, 0.0);
    // 2 m ahead and pulling away at 25 m/s: as close as it gets, and never a conflict.
    let t = table(vec![neighbour(0, Vec3::new(2.0, 0.0, 0.0), 30.0, 0.0, 0)]);
    assert!(app.on_neighbors(&mut ctx, EGO, 0, &t, &ego).is_empty());
    let s = app
        .evaluate(&ego, Vec3::new(2.0, 0.0, 0.0), 30.0, 0.0)
        .expect("it is a candidate; it is just not closing");
    assert!(s.ttc_s.is_nan());
    assert!(s.closing_mps < 0.0);
}

/// A vehicle in the next lane is not a rear-end conflict, however close it is and however
/// fast the ego is closing on it.
#[test]
fn a_vehicle_in_the_next_lane_is_not_a_rear_end_conflict() {
    let app = Fcw::default();
    let ego = belief(Vec3::ZERO, 30.0, 0.0);
    // 3.5 m to the left is one lane over; the corridor is one vehicle width.
    assert!(
        app.evaluate(&ego, Vec3::new(10.0, 3.5, 0.0), 0.0, 0.0)
            .is_none()
    );
    // Directly ahead, it is.
    assert!(
        app.evaluate(&ego, Vec3::new(10.0, 0.0, 0.0), 0.0, 0.0)
            .is_some()
    );
}

/// Two vehicles that reach the same point at the same moment have a post-encroachment time
/// of zero, and the intersection application says so.
#[test]
fn two_paths_that_cross_together_produce_a_zero_pet() {
    let rng = RngRegistry::new(13);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Ima::default();
    // The ego 30 m west of the junction at 10 m/s heading east; the peer 30 m south of it
    // at 10 m/s heading north. Both arrive in 3 s.
    let ego = belief(Vec3::new(-30.0, 0.0, 0.0), 10.0, 0.0);
    let t = table(vec![neighbour(
        0,
        Vec3::new(0.0, -30.0, 0.0),
        10.0,
        core::f64::consts::FRAC_PI_2,
        0,
    )]);
    let out = app.on_neighbors(&mut ctx, EGO, 0, &t, &ego);
    assert_eq!(out.len(), 1, "a crossing conflict: {out:?}");
    assert_eq!(out[0].surrogates.pet_s, 0.0);
    assert_eq!(out[0].surrogates.ttc_s, 3.0);
    // Three seconds to the conflict point is past the SSAM conflict threshold, so it is a
    // warning and not yet an imminent one.
    assert_eq!(out[0].severity, Severity::Warning);
    assert!(
        out[0].surrogates.required_decel_mps2.is_nan(),
        "a crossing conflict is not avoided by matching a leader's speed"
    );

    // Move the ego to 12 m out — 1.2 s — and it becomes imminent.
    let close = belief(Vec3::new(-12.0, 0.0, 0.0), 10.0, 0.0);
    let t2 = table(vec![neighbour(
        0,
        Vec3::new(0.0, -12.0, 0.0),
        10.0,
        core::f64::consts::FRAC_PI_2,
        0,
    )]);
    let out = app.on_neighbors(&mut ctx, EGO, NS_PER_MS, &t2, &close);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].severity, Severity::Imminent);
}

/// Two paths that cross at very different times are not a conflict: one road user has gone
/// long before the other arrives, which is exactly what a post-encroachment time measures.
#[test]
fn two_paths_that_cross_at_different_times_are_not_a_conflict() {
    let rng = RngRegistry::new(14);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Ima::default();
    // The ego is 30 m from the junction at 10 m/s (3 s away); the peer is 5 m from it at
    // 25 m/s (0.2 s away), so it is through 2.8 s before the ego arrives.
    let ego = belief(Vec3::new(-30.0, 0.0, 0.0), 10.0, 0.0);
    let t = table(vec![neighbour(
        0,
        Vec3::new(0.0, -5.0, 0.0),
        25.0,
        core::f64::consts::FRAC_PI_2,
        0,
    )]);
    assert!(
        app.on_neighbors(&mut ctx, EGO, 0, &t, &ego).is_empty(),
        "a 2.8 s post-encroachment time is not a conflict at the default threshold"
    );
    // But the measure itself is still computed and reported, which is what makes a
    // near-miss distribution possible rather than only the conflicts.
    let s = app
        .evaluate(
            &ego,
            Vec3::new(0.0, -5.0, 0.0),
            25.0,
            core::f64::consts::FRAC_PI_2,
        )
        .expect("the paths do cross");
    assert_eq!(s.pet_s, 2.8);
    assert!(
        s.ttc_s.is_finite(),
        "inside pet_conflict_s, so a TTC exists"
    );
}

/// Parallel paths never cross, so a platoon on a straight road produces no intersection
/// warnings at all.
#[test]
fn parallel_paths_never_produce_a_crossing_conflict() {
    let app = Ima::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);
    assert!(
        app.evaluate(&ego, Vec3::new(20.0, 0.0, 0.0), 20.0, 0.0)
            .is_none()
    );
    assert!(
        app.evaluate(&ego, Vec3::new(20.0, 3.5, 0.0), 15.0, 0.0)
            .is_none()
    );
    // And a stationary peer has no path to cross.
    assert!(
        app.evaluate(&ego, Vec3::new(0.0, -20.0, 0.0), 0.0, 1.0)
            .is_none()
    );
}

/// A peer ahead whose claimed speed falls by 0.4 g between two claims raises an EEBL
/// warning; one decelerating gently does not.
#[test]
fn a_peer_braking_at_point_four_g_raises_an_eebl_warning() {
    let rng = RngRegistry::new(15);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Eebl::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);

    // First claim: 20 m/s, 30 m ahead. No previous speed, so no estimate and no warning.
    let first = table(vec![neighbour(0, Vec3::new(30.0, 0.0, 0.0), 20.0, 0.0, 0)]);
    assert!(app.on_neighbors(&mut ctx, EGO, 0, &first, &ego).is_empty());

    // Second claim 100 ms later at 19.5 m/s: 5 m/s², above 0.4 g = 3.92 m/s².
    let second = table(vec![neighbour(
        0,
        Vec3::new(32.0, 0.0, 0.0),
        19.5,
        0.0,
        100 * NS_PER_MS,
    )]);
    let out = app.on_neighbors(&mut ctx, EGO, 100 * NS_PER_MS, &second, &ego);
    assert_eq!(out.len(), 1, "0.5 m/s in 100 ms is 5 m/s²: {out:?}");
    assert_eq!(out[0].kind, WarningKind::Issue);
    let decel = app
        .estimated_deceleration(&pseudo_signer(PEER, 0))
        .expect("an estimate");
    assert!((decel - 5.0).abs() < 1e-9, "{decel}");

    // A gentle deceleration does not fire: 0.1 m/s in 100 ms is 1 m/s².
    let mut gentle = Eebl::default();
    let a = table(vec![neighbour(0, Vec3::new(30.0, 0.0, 0.0), 20.0, 0.0, 0)]);
    gentle.on_neighbors(&mut ctx, EGO, 0, &a, &ego);
    let b = table(vec![neighbour(
        0,
        Vec3::new(32.0, 0.0, 0.0),
        19.9,
        0.0,
        100 * NS_PER_MS,
    )]);
    assert!(
        gentle
            .on_neighbors(&mut ctx, EGO, 100 * NS_PER_MS, &b, &ego)
            .is_empty()
    );
}

/// TS 103 759's plausibility window applied to the node's own estimate: two claims further
/// apart than 500 ms are not evidence about the instant claimed, so no deceleration is
/// estimated from them.
#[test]
fn a_deceleration_estimated_over_too_long_a_window_is_refused() {
    let rng = RngRegistry::new(16);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Eebl::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);
    assert_eq!(
        EeblParams::vsca().estimate_window,
        v2xw_core::time::Duration::from_millis(500)
    );

    let a = table(vec![neighbour(0, Vec3::new(30.0, 0.0, 0.0), 20.0, 0.0, 0)]);
    app.on_neighbors(&mut ctx, EGO, 0, &a, &ego);

    // The same 10 m/s speed drop, but claimed 600 ms later: outside the window.
    let b = table(vec![neighbour(
        0,
        Vec3::new(35.0, 0.0, 0.0),
        10.0,
        0.0,
        600 * NS_PER_MS,
    )]);
    assert!(
        app.on_neighbors(&mut ctx, EGO, 600 * NS_PER_MS, &b, &ego)
            .is_empty(),
        "a claim pair spanning 600 ms is outside the TS 103 759 window"
    );

    // Inside the window the same drop does fire, which is what makes the refusal above a
    // property of the window and not of the numbers.
    let mut inside = Eebl::default();
    inside.on_neighbors(&mut ctx, EGO, 0, &a, &ego);
    let c = table(vec![neighbour(
        0,
        Vec3::new(35.0, 0.0, 0.0),
        10.0,
        0.0,
        400 * NS_PER_MS,
    )]);
    assert_eq!(
        inside
            .on_neighbors(&mut ctx, EGO, 400 * NS_PER_MS, &c, &ego)
            .len(),
        1
    );
}

/// The whole set, on one scenario: three applications, records on one channel, and the
/// relevance scores the `on-demand` policy consumes.
#[test]
fn the_relevance_scores_reach_the_on_demand_policy() {
    let rng = RngRegistry::new(17);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut set = SafetyAppSet::default();
    assert_eq!(set.len(), 3);
    assert_eq!(
        set.ids(),
        vec![
            "safety-app/fcw-vsca".to_string(),
            "safety-app/ima-vsca".to_string(),
            "safety-app/eebl-vsca".to_string()
        ]
    );

    let ego = belief(Vec3::ZERO, 20.0, 0.0);
    let imminent = pseudo_signer(PEER, 0);
    let t = table(vec![
        // 15 m ahead, stationary: a 0.75 s time to collision.
        neighbour(0, Vec3::new(15.0, 0.0, 0.0), 0.0, 0.0, 0),
        // Far behind, going the other way: nothing any application cares about.
        neighbour(
            1,
            Vec3::new(-200.0, 0.0, 0.0),
            20.0,
            core::f64::consts::PI,
            0,
        ),
    ]);

    let out = set.run(&mut ctx, EGO, 0, &t, &ego);
    assert!(!out.is_empty());
    assert!(set.warnings() >= 1);

    // Every record went on `app.warning` and nowhere else.
    let channels: Vec<&str> = ctx.emitted().iter().map(|r| r.channel).collect();
    assert_eq!(channels.len(), out.len());
    assert!(channels.iter().all(|c| *c == "app.warning"));

    // The imminent subject scored, the irrelevant one did not, and the score is above the
    // shipped `on-demand` threshold of 0.5 — which is what makes the policy verify it.
    let score = set.relevance_of(&imminent).expect("the close peer scored");
    assert!(score > 0.5, "{score}");
    assert_eq!(score, relevance_from_ttc(0.75));
    assert!(set.relevance_of(&pseudo_signer(PEER, 1)).is_none());

    // And the map the runtime installs is keyed the way the runtime keys it.
    let map: &BTreeMap<[u8; 8], f64> = set.relevance();
    assert!(map.contains_key(&digest_key(&imminent)));
}

/// A revoked peer is not acted on: a node that has decided not to believe a sender does
/// not warn its driver about it.
#[test]
fn a_revoked_peer_raises_no_warning() {
    let rng = RngRegistry::new(18);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Fcw::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);
    let mut n = neighbour(0, Vec3::new(15.0, 0.0, 0.0), 0.0, 0.0, 0);
    n.state = VerificationState::Revoked;
    assert!(
        app.on_neighbors(&mut ctx, EGO, 0, &table(vec![n]), &ego)
            .is_empty()
    );

    // The same peer, verified, does warn — so the exclusion is about the state and not
    // about the geometry.
    let ok = neighbour(0, Vec3::new(15.0, 0.0, 0.0), 0.0, 0.0, 0);
    assert_eq!(
        app.on_neighbors(&mut ctx, EGO, NS_PER_MS, &table(vec![ok]), &ego)
            .len(),
        1
    );
}

/// An unverified peer *is* acted on, and that is deliberate: the cost of the `on-demand`
/// policy in safety terms is the warnings an unverified message caused, not their absence.
#[test]
fn an_unverified_peer_is_acted_on() {
    let rng = RngRegistry::new(19);
    let mut ctx = NodeRuntimeCtx::new(0, &rng);
    let mut app = Fcw::default();
    let ego = belief(Vec3::ZERO, 20.0, 0.0);
    let mut n = neighbour(0, Vec3::new(15.0, 0.0, 0.0), 0.0, 0.0, 0);
    n.state = VerificationState::Unverified;
    assert_eq!(
        app.on_neighbors(&mut ctx, EGO, 0, &table(vec![n]), &ego)
            .len(),
        1
    );
}

/// Every application charges its work to a named cost class, so a safety layer cannot be
/// free.
#[test]
fn every_application_charges_its_work() {
    let set = SafetyAppSet::default();
    let costs = set.costs();
    assert_eq!(costs.len(), 3);
    for (c, id) in costs.iter().zip(set.ids()) {
        assert_eq!(c.op, id, "the descriptor names the model it charges for");
        assert_eq!(c.class, v2xw_node::server::OpClass::Application);
    }
}

/// Two runs over the same neighbourhood produce the same warnings in the same order, which
/// is what the digest-ordered neighbour table buys.
#[test]
fn the_warnings_are_deterministic_in_order() {
    fn run(insert_reversed: bool) -> Vec<(String, f64)> {
        let rng = RngRegistry::new(20);
        let mut ctx = NodeRuntimeCtx::new(0, &rng);
        let mut set = SafetyAppSet::new(vec![Box::new(Fcw::default())]);
        let ego = belief(Vec3::ZERO, 25.0, 0.0);
        let mut entries = vec![
            neighbour(0, Vec3::new(12.0, 0.0, 0.0), 0.0, 0.0, 0),
            neighbour(1, Vec3::new(20.0, 0.0, 0.0), 0.0, 0.0, 0),
            neighbour(2, Vec3::new(8.0, 0.0, 0.0), 0.0, 0.0, 0),
        ];
        if insert_reversed {
            entries.reverse();
        }
        set.run(&mut ctx, EGO, 0, &table(entries), &ego)
            .into_iter()
            .map(|w| {
                (
                    v2xw_node::safety::digest_hex(&w.subject),
                    w.surrogates.ttc_s,
                )
            })
            .collect()
    }
    let a = run(false);
    assert!(a.len() >= 2, "several peers must warn: {a:?}");
    assert_eq!(a, run(true), "insertion order must not change the output");
    let mut sorted = a.clone();
    sorted.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(a, sorted, "the warnings come out in digest order");
}
