//! §10.3 — poses and state. Items Q1, Q4 and Q6.
//!
//! Q2 (no quantisation drift) and Q3 (the teleport escape) are owned by
//! `crates/v2xw-record/tests/drift.rs`, Q5 (slot cooling-off) by
//! `crates/v2xw-record/tests/conformance.rs`, and Q7 binds the client's draw path.

use v2xw_record::quant::{
    ACCEL_SCALE, BRAD_PER_TURN, SPEED_SCALE, accel_cq, brad_to_rad, heading_brad,
    round_half_away_from_zero, speed_cq, x_mm, z_cm,
};
use v2xw_record::wire::snapshot::{
    ActorRow, KeyframeBody, PROFILE_FULL, ST_ATTACKER, ST_EQUIPPED, ST_REPORTED, ST_REVOKED,
    ST_TRANSMITTING, SignalRow,
};

/// **Q1** — "Quantisation matches §3.2 exactly, including half-away-from-zero rounding and
/// the brad wrap formula."
///
/// The §9 worked vectors are reproduced byte for byte by
/// `crates/v2xw-record/tests/wire_vectors.rs`; this checks the two *rules* those vectors
/// are instances of, which is what a second implementation needs in order to agree.
#[test]
fn q1_quantisation_matches_the_normative_rules() {
    // Half away from zero, not to even. A banker's-rounding implementation gives 2.0 and
    // 2.0 for the last two, which is the difference this assertion exists to catch.
    assert_eq!(round_half_away_from_zero(0.5), 1.0);
    assert_eq!(round_half_away_from_zero(-0.5), -1.0);
    assert_eq!(round_half_away_from_zero(1.5), 2.0);
    assert_eq!(round_half_away_from_zero(2.5), 3.0);
    assert_eq!(round_half_away_from_zero(-2.5), -3.0);

    // Positions are millimetres from the run origin.
    assert_eq!(x_mm(0.0, -500.0), 500_000);
    assert_eq!(x_mm(-500.0, -500.0), 0);
    assert_eq!(x_mm(1.234, 0.0), 1234);
    assert_eq!(x_mm(-1.234, 0.0), -1234);
    // The half case is asserted on the rounding rule itself above rather than through a
    // decimal literal: 0.0005 is not exactly representable in binary, so a test that fed
    // one in would be asserting on the literal's representation error, not on §3.2.
    assert_eq!(z_cm(1.0, 0.0), 100);
    assert_eq!(z_cm(-1.0, 0.0), -100);

    // The brad wrap. A full turn is exactly representable and wraps to zero, which is the
    // property that removes the ±π branch a signed heading would need.
    assert_eq!(heading_brad(0.0), 0);
    assert_eq!(heading_brad(core::f64::consts::PI), 32_768);
    assert_eq!(heading_brad(core::f64::consts::TAU), 0, "a full turn wraps");
    assert_eq!(
        heading_brad(-core::f64::consts::FRAC_PI_2),
        49_152,
        "a quarter turn clockwise is three quarters anticlockwise"
    );
    assert_eq!(
        heading_brad(3.0 * core::f64::consts::TAU + core::f64::consts::PI),
        32_768,
        "three and a half turns is half a turn"
    );
    assert_eq!(BRAD_PER_TURN, 65_536.0);

    // Decoding is the inverse, on the decoded grid.
    for brad in [0u16, 1, 16_384, 32_768, 49_152, 65_535] {
        assert_eq!(
            heading_brad(brad_to_rad(brad)),
            brad,
            "brad {brad} did not survive a decode and re-encode"
        );
    }

    // Speed and acceleration are fixed-point at the scales §3.2 fixes.
    assert_eq!(SPEED_SCALE, 128.0);
    assert_eq!(ACCEL_SCALE, 64.0);
    assert_eq!(speed_cq(1.0), 128);
    assert_eq!(speed_cq(-1.0), -128);
    assert_eq!(accel_cq(1.0), 64);
    assert_eq!(
        speed_cq(1.0e9),
        i16::MAX,
        "an out-of-range value saturates rather than wrapping"
    );
    assert_eq!(speed_cq(f64::NAN), 0, "a non-finite value maps to zero");
}

/// **Q4** — "Keyframe actor rows are indexed by slot, with `actor_id = 0xFFFFFFFF` for
/// empty slots."
///
/// Built by hand with a hole in the middle rather than taken from the fixture, because the
/// property is precisely that the *index* is the slot: a run whose actors happen to be
/// contiguous cannot distinguish "indexed by slot" from "listed in order".
#[test]
fn q4_keyframe_actor_rows_are_indexed_by_slot() {
    assert_eq!(ActorRow::EMPTY.actor_id, 0xFFFF_FFFF);
    assert!(!ActorRow::EMPTY.is_occupied());

    let occupied = |actor_id: u32, x_mm: i32| ActorRow {
        actor_id,
        x_mm,
        y_mm: 0,
        lane_id: 0xFFFF_FFFF,
        z_cm: 0,
        heading_brad: 0,
        speed_cq: 0,
        accel_cq: 0,
        class_idx: 0,
        state: ST_EQUIPPED,
        verified_neighbors: 0,
        flags8: 0,
    };

    let body = KeyframeBody {
        sim_time_ns: 1_000_000_000,
        origin: [-500.0, -500.0, 0.0],
        gop_index: 3,
        profile: PROFILE_FULL,
        actors: vec![
            occupied(10, 1_000),
            ActorRow::EMPTY,
            occupied(12, 3_000),
            ActorRow::EMPTY,
            occupied(14, 5_000),
        ],
        signals: vec![SignalRow {
            signal_id: 1,
            time_to_change_ds: 25,
            phase: 3,
        }],
    };

    let decoded = KeyframeBody::decode(&body.encode()).expect("the keyframe round-trips");
    assert_eq!(decoded, body, "the encoding is not lossless");
    assert_eq!(decoded.actors.len(), 5, "empty slots still occupy a row");
    for (slot, row) in decoded.actors.iter().enumerate() {
        if slot % 2 == 0 {
            assert!(row.is_occupied(), "slot {slot} lost its actor");
            assert_eq!(
                row.actor_id,
                10 + u32::try_from(slot).expect("small"),
                "slot {slot} holds the wrong actor, so the index is not the slot"
            );
        } else {
            assert_eq!(
                *row,
                ActorRow::EMPTY,
                "slot {slot} must be the empty sentinel, not a shifted neighbour"
            );
        }
    }
}

/// **Q6** — "`state` bit semantics match §3.3.4; 'benign' is the absence of bits 0–2."
#[test]
fn q6_the_state_byte_bits_and_the_meaning_of_benign() {
    assert_eq!(ST_ATTACKER, 0x01);
    assert_eq!(ST_REPORTED, 0x02);
    assert_eq!(ST_REVOKED, 0x04);
    assert_eq!(ST_EQUIPPED, 0x08);
    assert_eq!(ST_TRANSMITTING, 0x10);

    let benign = |state: u8| state & (ST_ATTACKER | ST_REPORTED | ST_REVOKED) == 0;
    assert!(benign(0));
    assert!(benign(ST_EQUIPPED | ST_TRANSMITTING));
    // Each of the three bits alone is enough to stop a vehicle being benign: a renderer
    // that only looked at ST_ATTACKER would draw a revoked node as ordinary traffic.
    assert!(!benign(ST_ATTACKER));
    assert!(!benign(ST_REPORTED));
    assert!(!benign(ST_REVOKED));
    assert!(!benign(ST_EQUIPPED | ST_REVOKED));
}
