//! The privacy observer: it links what it can, declines what it cannot, and reports both —
//! because a linkability rate whose denominator is invisible is a number that can only go
//! up.
//!
//! The reference result these cases are shaped around is Wiedersheim et al. (WONS 2010),
//! cited in 07-threats-and-detection.md §6: at 1 Hz beacons, a pseudonym change with no
//! silence is trivially linkable, and a long enough silent period is what defeats a
//! kinematic tracker. Both directions are asserted, because a model that could only
//! demonstrate one of them would be measuring its own bias.

use v2xw_core::ids::NodeId;
use v2xw_core::time::{NS_PER_S, SimTime, secs_to_ns};
use v2xw_threat::ctx::CollectingCtx;
use v2xw_threat::obs::{
    ObservedKind, ObservedMessage, SelfBelief, StationType, VerificationState,
};
use v2xw_threat::privacy::{METHOD_UNLINKED, ObserverParams, PrivacyObserver};
use v2xw_threat::records::{PrivacyLinkClaim, PrivacyTrackSegment};

const OBS: NodeId = NodeId::new(9);
const A: [u8; 8] = [0xAA; 8];
const B: [u8; 8] = [0xBB; 8];
const C: [u8; 8] = [0xCC; 8];

fn me(t: SimTime) -> SelfBelief {
    SelfBelief {
        node: OBS,
        believed_time: t,
        x_m: 0.0,
        y_m: 0.0,
        radio_range_m: 1_000.0,
    }
}

fn beacon(signer: [u8; 8], x_m: f64, y_m: f64, t: SimTime) -> ObservedMessage {
    ObservedMessage {
        signer,
        kind: ObservedKind::Beacon,
        received_at: t,
        claimed_generation_time: t,
        claimed_x_m: x_m,
        claimed_y_m: y_m,
        claimed_speed_mps: 15.0,
        claimed_heading_rad: 0.0,
        claimed_pos_confidence_m: 2.0,
        repetitions: 1,
        cert_valid_from: 0,
        cert_valid_to: u64::MAX,
        station_type: StationType::Vehicle,
        verification: VerificationState::Valid,
    }
}

/// Feeds one reception and returns whatever the observer concluded.
fn feed(
    o: &mut PrivacyObserver,
    ctx: &mut CollectingCtx,
    m: &ObservedMessage,
) -> Option<v2xw_threat::LinkOutcome> {
    ctx.set_now(m.received_at);
    o.on_message(ctx, &me(m.received_at), m)
}

fn decode<T: serde::de::DeserializeOwned>(ctx: &CollectingCtx, channel: &str) -> Vec<T> {
    ctx.on_channel(channel)
        .iter()
        .map(|r| serde_json::from_slice::<T>(&r.json).expect("the record decodes"))
        .collect()
}

fn links(ctx: &CollectingCtx) -> Vec<PrivacyLinkClaim> {
    decode(ctx, "privacy.link")
}

fn tracks(ctx: &CollectingCtx) -> Vec<PrivacyTrackSegment> {
    decode(ctx, "privacy.track")
}

// ---------------------------------------------------------------------------------------
// Linking, and failing to
// ---------------------------------------------------------------------------------------

#[test]
fn a_pseudonym_change_with_no_silence_is_linked() {
    // One vehicle at 1 Hz, driving straight east, changes pseudonym at t = 4 s. The
    // constant-velocity prediction lands on the new claim, so the change buys nothing —
    // which is the WONS 2010 finding.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    let outcome = feed(&mut o, &mut ctx, &beacon(B, 60.0, 0.0, 4 * NS_PER_S))
        .expect("a first reception from B is a linkage decision");
    assert_eq!(
        outcome.predecessor.as_deref(),
        Some("aaaaaaaaaaaaaaaa"),
        "the observer did not follow the change"
    );
    assert_eq!(outcome.posterior, 1.0, "nobody else to confuse it with");
    assert_eq!(outcome.anonymity_set_size, 1);
    assert_eq!(
        outcome.degree_of_anonymity, 0.0,
        "|Psi| = 1 is no anonymity at all"
    );
    assert_eq!(o.links_claimed(), 1);
    assert_eq!(o.links_declined(), 1, "A's own first sighting");
    // The chain is now B's, and it carries A's start.
    let (chain_links, duration_s) = o.chain_of(&B).expect("B is tracked");
    assert_eq!(chain_links, 1);
    assert_eq!(duration_s, 4.0);
    // A is retired into the chain rather than left behind as a second track.
    assert_eq!(o.tracked(), 1);
    assert!(o.chain_of(&A).is_none());
}

#[test]
fn a_long_enough_silent_period_defeats_the_observer() {
    // The same vehicle, silent for 20 s — longer than the observer's 13 s gate, which is
    // PRESERVE's longest cited silent period. The new pseudonym starts a new chain.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    let outcome = feed(
        &mut o,
        &mut ctx,
        &beacon(B, 45.0 + 15.0 * 20.0, 0.0, 23 * NS_PER_S),
    )
    .expect("a decision");
    assert_eq!(outcome.predecessor, None, "it bridged a 20 s silence");
    assert_eq!(outcome.anonymity_set_size, 0);
    assert_eq!(o.links_claimed(), 0);
    let (chain_links, _) = o.chain_of(&B).unwrap();
    assert_eq!(chain_links, 0, "a new chain, not a continuation");
}

#[test]
fn a_teleporting_claim_is_outside_the_gate() {
    // Even with no silence: a new pseudonym that appears 500 m from the prediction is not
    // the same vehicle by any kinematic argument the observer can make.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    let outcome = feed(&mut o, &mut ctx, &beacon(B, 560.0, 0.0, 4 * NS_PER_S)).unwrap();
    assert_eq!(outcome.predecessor, None);
}

#[test]
fn a_crowd_leaves_the_observer_uncertain() {
    // Two vehicles two metres apart, both predicted into the same place. The observer must
    // still pick one — it is an adversary, not a referee — but the degree of anonymity says
    // the pick was a coin flip, and that is the number the pseudonym scheme is trying to
    // move.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        let t = i * NS_PER_S;
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, t));
        feed(&mut o, &mut ctx, &beacon(B, 15.0 * i as f64, 2.0, t));
    }
    let outcome = feed(&mut o, &mut ctx, &beacon(C, 60.0, 1.0, 4 * NS_PER_S)).unwrap();
    assert_eq!(outcome.anonymity_set_size, 2);
    assert!(
        (outcome.posterior - 0.5).abs() < 1e-3,
        "posterior {} is not a coin flip",
        outcome.posterior
    );
    assert!(
        (outcome.effective_anonymity_set_bits - 1.0).abs() < 1e-3,
        "S = {} bits, expected 1 for two equal candidates",
        outcome.effective_anonymity_set_bits
    );
    assert!(
        (outcome.degree_of_anonymity - 1.0).abs() < 1e-3,
        "d = {}, expected 1 for a uniform posterior over two",
        outcome.degree_of_anonymity
    );
    // The tie goes to the lower digest, deterministically.
    assert_eq!(outcome.predecessor.as_deref(), Some("aaaaaaaaaaaaaaaa"));
}

#[test]
fn a_pseudonym_heard_at_the_same_instant_cannot_be_the_predecessor() {
    // One radio is not two pseudonyms at one instant. A Sybil attacker breaks exactly this
    // assumption, which is why a Sybil run's linkability is not comparable with an honest
    // run's — and the observer excludes same-instant candidates rather than linking a
    // vehicle to its neighbour.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    feed(&mut o, &mut ctx, &beacon(A, 0.0, 0.0, 0));
    let outcome = feed(&mut o, &mut ctx, &beacon(B, 0.0, 0.0, 0)).unwrap();
    assert_eq!(outcome.predecessor, None);
}

// ---------------------------------------------------------------------------------------
// The records
// ---------------------------------------------------------------------------------------

#[test]
fn the_observer_emits_a_record_when_it_declines() {
    // Otherwise the denominator of a linkability rate is invisible and the rate can only
    // ever be 1.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    feed(&mut o, &mut ctx, &beacon(A, 0.0, 0.0, 0));
    let claims = links(&ctx);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].method, METHOD_UNLINKED);
    assert!(claims[0].predecessor.is_empty());
    assert_eq!(claims[0].successor, "aaaaaaaaaaaaaaaa");
    assert_eq!(claims[0].posterior, 0.0);
    assert_eq!(claims[0].observer, OBS);
}

#[test]
fn a_linked_change_is_a_record_with_two_digests_and_a_posterior() {
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    feed(&mut o, &mut ctx, &beacon(B, 60.0, 0.0, 4 * NS_PER_S));
    let claims = links(&ctx);
    assert_eq!(claims.len(), 2, "A's first sighting, then the change");
    let linked = &claims[1];
    assert_eq!(linked.predecessor, "aaaaaaaaaaaaaaaa");
    assert_eq!(linked.successor, "bbbbbbbbbbbbbbbb");
    assert_eq!(linked.posterior, 1.0);
    assert_eq!(linked.t, 4 * NS_PER_S);
    // Quantised at the writer (build decision D9).
    for value in [
        linked.posterior,
        linked.effective_anonymity_set_bits,
        linked.degree_of_anonymity,
    ] {
        assert!(v2xw_core::math::is_on_grid(
            value,
            v2xw_threat::records::SCORE_Q
        ));
    }
}

#[test]
fn a_chain_the_observer_gives_up_on_is_a_tracking_duration_sample() {
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    feed(&mut o, &mut ctx, &beacon(B, 60.0, 0.0, 4 * NS_PER_S));
    // Twenty seconds of silence: the observer gives up.
    o.sweep(&mut ctx, 30 * NS_PER_S);
    let segments = tracks(&ctx);
    assert_eq!(segments.len(), 1);
    let seg = &segments[0];
    assert_eq!(seg.origin, "aaaaaaaaaaaaaaaa");
    assert_eq!(seg.last, "bbbbbbbbbbbbbbbb");
    assert_eq!(seg.links, 1, "one pseudonym change followed");
    assert_eq!(seg.duration_s, 4.0);
    assert_eq!(seg.fixes, 5);
    assert_eq!(seg.closed_reason, "silence");
    assert_eq!(o.chains_closed(), 1);
    assert_eq!(o.tracked(), 0);
    assert_eq!(o.longest_chain_links(), 1);
    assert_eq!(o.longest_track_s(), 4.0);
}

#[test]
fn finishing_a_run_closes_the_last_chains() {
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    feed(&mut o, &mut ctx, &beacon(A, 0.0, 0.0, 0));
    feed(&mut o, &mut ctx, &beacon(B, 500.0, 0.0, 0));
    assert!(tracks(&ctx).is_empty(), "nothing has ended yet");
    o.finish(&mut ctx);
    let segments = tracks(&ctx);
    assert_eq!(segments.len(), 2);
    for seg in &segments {
        assert_eq!(seg.closed_reason, "run-end");
        assert_eq!(seg.links, 0);
    }
    assert_eq!(o.tracked(), 0);
}

#[test]
fn the_observer_writes_only_on_its_own_channels_and_never_on_a_ground_truth_one() {
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    feed(&mut o, &mut ctx, &beacon(B, 60.0, 0.0, 4 * NS_PER_S));
    o.finish(&mut ctx);
    assert!(ctx.on_channel("gt.attack.action").is_empty());
    assert!(ctx.on_channel("node.tx").is_empty(), "it transmits nothing");
    assert!(ctx.on_channel("det.observation").is_empty(), "it accuses nobody");
    assert_eq!(
        ctx.records().len(),
        links(&ctx).len() + tracks(&ctx).len(),
        "it writes nothing else"
    );
}

// ---------------------------------------------------------------------------------------
// Verification states and determinism
// ---------------------------------------------------------------------------------------

#[test]
fn the_four_verification_states_are_four_different_facts_to_the_observer() {
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);

    // Deferred: counted, and still tracked — a listener has no verification policy of its
    // own, and "not checked" says nothing about trackability.
    let mut deferred = beacon(A, 0.0, 0.0, 0);
    deferred.verification = VerificationState::Unverified;
    feed(&mut o, &mut ctx, &deferred);
    assert_eq!(o.deferred(), 1);
    assert_eq!(o.tracked(), 1);
    assert_eq!(o.skipped_unverifiable(), 0);

    // Bad signature and unknown certificate: tracked under the default policy.
    let mut bad = beacon(B, 100.0, 0.0, NS_PER_S);
    bad.verification = VerificationState::BadSignature;
    feed(&mut o, &mut ctx, &bad);
    let mut unknown = beacon(C, 200.0, 0.0, NS_PER_S);
    unknown.verification = VerificationState::UnknownCertificate;
    feed(&mut o, &mut ctx, &unknown);
    assert_eq!(o.tracked(), 3);
    assert_eq!(o.skipped_unverifiable(), 0);
    assert_eq!(o.deferred(), 1, "a deferred check is not a failed one");
}

#[test]
fn an_observer_that_only_follows_verified_traffic_ignores_a_decoy() {
    // The other policy: what a decoy transmitter buys a vehicle's privacy is the
    // difference between this run and the one above.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::new(
        OBS,
        ObserverParams {
            track_unverifiable: false,
            ..ObserverParams::default()
        },
    );
    let mut bad = beacon(A, 0.0, 0.0, 0);
    bad.verification = VerificationState::BadSignature;
    assert!(feed(&mut o, &mut ctx, &bad).is_none());
    assert_eq!(o.tracked(), 0);
    assert_eq!(o.skipped_unverifiable(), 1);
    assert!(links(&ctx).is_empty());
}

#[test]
fn two_observers_with_the_same_receptions_reach_the_same_conclusions() {
    // No RNG anywhere in the observer, and every iteration in digest order, so this is a
    // property of the code rather than of the seed.
    let run = || {
        let mut ctx = CollectingCtx::new(3);
        let mut o = PrivacyObserver::cited_defaults(OBS);
        for i in 0..4u64 {
            let t = i * NS_PER_S;
            feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, t));
            feed(&mut o, &mut ctx, &beacon(B, 15.0 * i as f64, 2.0, t));
        }
        feed(&mut o, &mut ctx, &beacon(C, 60.0, 1.0, 4 * NS_PER_S));
        o.finish(&mut ctx);
        (links(&ctx), tracks(&ctx))
    };
    assert_eq!(run(), run());
}

#[test]
fn the_observer_never_draws_from_the_generator() {
    let card = v2xw_threat::privacy::card(&ObserverParams::default());
    assert!(!card.determinism.uses_rng);
    assert!(card.determinism.rng_domains.is_empty());
    card.validate().unwrap();
}

#[test]
fn a_short_silence_inside_the_gate_is_still_bridged() {
    // The gate is the most consequential parameter in the model, so both sides of it are
    // asserted: 5 s of silence is inside PRESERVE's 3-13 s band and is bridged.
    let mut ctx = CollectingCtx::new(1);
    let mut o = PrivacyObserver::cited_defaults(OBS);
    for i in 0..4u64 {
        feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
    }
    // Five seconds later, where a constant-velocity prediction says it should be.
    let t = 8 * NS_PER_S;
    let outcome = feed(&mut o, &mut ctx, &beacon(B, 45.0 + 15.0 * 5.0, 0.0, t)).unwrap();
    assert_eq!(outcome.predecessor.as_deref(), Some("aaaaaaaaaaaaaaaa"));
    // And the gate has grown with the silence, which is what lets it be bridged at all.
    assert!(outcome.posterior > 0.0);
}

#[test]
fn the_gate_grows_with_the_silence_but_not_without_limit() {
    // A claim 40 m off the prediction after 5 s of silence: inside the grown gate. The
    // same 40 m error after 1 s is outside it. That asymmetry is the covariance growth,
    // and without it every pseudonym change would link.
    let build = |gap_s: f64, offset_m: f64| {
        let mut ctx = CollectingCtx::new(1);
        let mut o = PrivacyObserver::cited_defaults(OBS);
        for i in 0..4u64 {
            feed(&mut o, &mut ctx, &beacon(A, 15.0 * i as f64, 0.0, i * NS_PER_S));
        }
        let t = 3 * NS_PER_S + secs_to_ns(gap_s);
        let predicted = 45.0 + 15.0 * gap_s;
        feed(
            &mut o,
            &mut ctx,
            &beacon(B, predicted + offset_m, 0.0, t),
        )
        .unwrap()
        .predecessor
        .is_some()
    };
    assert!(!build(1.0, 40.0), "40 m off after 1 s must not link");
    assert!(build(5.0, 40.0), "40 m off after 5 s is inside the grown gate");
}
