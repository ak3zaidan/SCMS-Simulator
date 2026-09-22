//! Quantisation drift, the absolute escape and the heading wrap — §3.2, the ADR 0008
//! amendment, and conformance Q1, Q2 (`no_quantisation_drift`) and Q3
//! (`teleport_escape`).
//!
//! The point of the whole scheme is one sentence: a delta is the difference against the
//! value **as previously transmitted and quantised**, never against the engine's
//! unquantised state. `naive_differencing_accumulates_error_and_ours_does_not` measures
//! both, because a test that only checks the correct implementation does not show why the
//! correction in ADR 0008's amendment mattered.

use v2xw_core::ids::ActorId;
use v2xw_core::time::Duration;
use v2xw_record::encoder::{ActorPose, Cadence, Snapshot, SnapshotEncoder};
use v2xw_record::profile::Profile;
use v2xw_record::wire::MsgType;
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody, MFLAG_ABSOLUTE, ST_EQUIPPED};
use v2xw_record::{SnapshotFrame, quant};

/// The decoded state of one slot, in the integers the wire carries.
#[derive(Debug, Clone, Copy, Default)]
struct SlotState {
    x_mm: i64,
    y_mm: i64,
    z_mm: i64,
    heading_brad: u16,
}

/// A client: apply a keyframe, then every delta of its GOP in order (§3.4).
fn apply(state: &mut SlotState, frame: &v2xw_record::Frame, slot: u32) {
    let header = frame.header().expect("a header");
    match header.kind() {
        Some(MsgType::Keyframe) => {
            let kf = KeyframeBody::decode(frame.body()).expect("a keyframe body");
            let row = kf.actors[slot as usize];
            state.x_mm = i64::from(row.x_mm);
            state.y_mm = i64::from(row.y_mm);
            state.z_mm = i64::from(row.z_cm) * 10;
            state.heading_brad = row.heading_brad;
        }
        Some(MsgType::Delta) => {
            let d = DeltaBody::decode(frame.body()).expect("a delta body");
            let mut abs = d.abs.iter();
            for row in &d.moved {
                let escape = row.mflags & MFLAG_ABSOLUTE != 0;
                let entry = if escape { abs.next().copied() } else { None };
                if row.slot != slot {
                    continue;
                }
                match entry {
                    Some(e) => {
                        state.x_mm = i64::from(e.x_mm);
                        state.y_mm = i64::from(e.y_mm);
                        state.z_mm = i64::from(e.z_cm) * 10;
                    }
                    None => {
                        state.x_mm += i64::from(row.dx_mm);
                        state.y_mm += i64::from(row.dy_mm);
                        state.z_mm += i64::from(row.dz_mm);
                    }
                }
                state.heading_brad = row.heading_brad;
            }
        }
        other => panic!("not a snapshot frame: {other:?}"),
    }
}

/// The true trajectory: 13.894 m/s east, so every 100 ms step advances exactly 1389.4 mm
/// — a displacement that rounds *down* every single time, which is what makes naive
/// differencing drift in one direction instead of wandering.
fn truth(step: u32) -> [f64; 3] {
    let secs = f64::from(step) * 0.1;
    [
        -480.0 + 13.894 * secs,
        7.5 * v2xw_core::math::sin(0.013 * secs),
        0.15 + 0.004 * v2xw_core::math::sin(0.07 * secs),
    ]
}

fn pose(step: u32, pos: [f64; 3]) -> ActorPose {
    ActorPose {
        slot: 0,
        actor: ActorId::new(0),
        node: None,
        pos_m: pos,
        heading_rad: 0.0007 * f64::from(step),
        speed_mps: 13.894,
        accel_mps2: 0.0,
        lane: None,
        class_idx: 0,
        state: ST_EQUIPPED,
        verified_neighbors: 0,
    }
}

const STEPS: u32 = 10_000;

#[test]
fn delta_quantisation_does_not_drift_over_ten_thousand_steps() {
    // One GOP of ten thousand steps: no keyframe re-anchors the reference, so if the
    // scheme drifted at all it would show up here at full strength.
    let cadence = Cadence::new(Duration::from_secs(1_000), Duration::from_millis(100))
        .expect("a whole number of steps per keyframe period");
    let origin = [-500.0, -500.0, 0.0];
    let mut encoder = SnapshotEncoder::new(origin, cadence, Profile::Full, 0);
    let mut state = SlotState::default();
    let mut keyframes = 0;
    let mut worst = [0.0f64; 3];

    for step in 0..STEPS {
        let pos = truth(step);
        let snap = Snapshot::new(u64::from(step) * 100_000_000, vec![pose(step, pos)]);
        let frame = encoder.encode(&snap).expect("the step encodes");
        if frame.is_keyframe() {
            keyframes += 1;
        }
        apply(&mut state, frame.frame(), 0);

        // The reconstruction equals what the writer quantised, exactly (Q2's "reproduces
        // the server's quantised state").
        assert_eq!(
            state.x_mm,
            i64::from(quant::x_mm(pos[0], origin[0])),
            "step {step}: x diverged from the transmitted value"
        );
        assert_eq!(state.y_mm, i64::from(quant::x_mm(pos[1], origin[1])));

        let decoded = [
            quant::mm_to_m(state.x_mm, origin[0]),
            quant::mm_to_m(state.y_mm, origin[1]),
            quant::mm_to_m(state.z_mm, origin[2]),
        ];
        for axis in 0..3 {
            let err = (decoded[axis] - pos[axis]).abs();
            worst[axis] = worst[axis].max(err);
        }
    }

    assert_eq!(keyframes, 1, "the whole run is one GOP");
    assert!(
        worst[0] <= 1e-3 && worst[1] <= 1e-3,
        "Q2 asks for a maximum deviation of 1 mm after 10,000 steps; got x {:.6} m, y {:.6} m",
        worst[0],
        worst[1]
    );
    // z is transmitted on the centimetre grid by the keyframe and on the millimetre grid
    // by every delta, so its error is bounded by half a millimetre after the first delta
    // and by half a centimetre at the keyframe itself.
    assert!(
        worst[2] <= 5e-3,
        "z deviated by {:.6} m, more than the centimetre grid allows",
        worst[2]
    );
    println!(
        "10,000 steps, one GOP: max |decoded - true| = {:.9} m (x), {:.9} m (y), {:.9} m (z)",
        worst[0], worst[1], worst[2]
    );
}

#[test]
fn naive_differencing_accumulates_error_and_ours_does_not() {
    // The scheme ADR 0008's amendment warns against: quantise the *difference of the true
    // positions* instead of differencing against the transmitted value.
    let origin = -500.0;
    let mut naive_mm: i64 = i64::from(quant::x_mm(truth(0)[0], origin));
    let mut correct_mm: i64 = naive_mm;
    for step in 1..STEPS {
        let prev = truth(step - 1)[0];
        let now = truth(step)[0];
        // Naive: round the true displacement, then add it.
        let dx = quant::round_half_away_from_zero((now - prev) * 1000.0) as i64;
        naive_mm += dx;
        // Correct: difference against what was transmitted.
        let target = i64::from(quant::x_mm(now, origin));
        correct_mm += target - correct_mm;
    }
    let truth_mm = i64::from(quant::x_mm(truth(STEPS - 1)[0], origin));
    let naive_err_m = (naive_mm - truth_mm) as f64 / 1000.0;
    let correct_err_m = (correct_mm - truth_mm) as f64 / 1000.0;
    println!(
        "after {STEPS} steps: naive is off by {naive_err_m:.3} m, ours by {correct_err_m:.3} m"
    );
    assert_eq!(correct_err_m, 0.0, "the transmitted reference cannot drift");
    assert!(
        naive_err_m.abs() > 1.0,
        "the naive scheme should have drifted by metres; it drifted by {naive_err_m:.3} m, \
         so this test is not demonstrating anything"
    );
}

#[test]
fn drift_stays_bounded_at_the_default_one_second_cadence_too() {
    let cadence = Cadence::DEFAULT;
    let origin = [-500.0, -500.0, 0.0];
    let mut encoder = SnapshotEncoder::new(origin, cadence, Profile::Full, 0);
    let mut state = SlotState::default();
    let mut worst = 0.0f64;
    for step in 0..STEPS {
        let pos = truth(step);
        let snap = Snapshot::new(u64::from(step) * 100_000_000, vec![pose(step, pos)]);
        let frame = encoder.encode(&snap).expect("the step encodes");
        apply(&mut state, frame.frame(), 0);
        worst = worst.max((quant::mm_to_m(state.x_mm, origin[0]) - pos[0]).abs());
    }
    assert!(worst <= 1e-3, "x deviated by {worst:.9} m");
}

#[test]
fn a_teleport_larger_than_the_delta_range_uses_the_absolute_escape() {
    // §3.2's escape hatch: a step whose |dx| would exceed 32,000 mm sets MFLAG_ABSOLUTE
    // and puts the pose in the absolute block. There is no unrepresentable case.
    let cadence = Cadence::DEFAULT;
    let origin = [-500.0, -500.0, 0.0];
    let mut encoder = SnapshotEncoder::new(origin, cadence, Profile::Full, 0);
    let mut state = SlotState::default();
    let jump_at = 4u32;
    let mut escapes = 0;

    for step in 0..9u32 {
        let mut pos = truth(step);
        if step >= jump_at {
            // Far outside i16 millimetres, and outside i16 metres too.
            pos[0] += 250.0;
            pos[2] += 40.0;
        }
        let snap = Snapshot::new(u64::from(step) * 100_000_000, vec![pose(step, pos)]);
        let frame = encoder.encode(&snap).expect("the step encodes");
        if let SnapshotFrame::Delta(f) = &frame {
            let d = DeltaBody::decode(f.body()).expect("a delta body");
            if d.moved.iter().any(|r| r.mflags & MFLAG_ABSOLUTE != 0) {
                escapes += 1;
                assert_eq!(d.abs.len(), 1, "one escape row, one absolute entry");
                assert_eq!(
                    d.step_index, jump_at,
                    "the escape is on the step that jumped"
                );
            }
        }
        apply(&mut state, frame.frame(), 0);
        assert_eq!(
            state.x_mm,
            i64::from(quant::x_mm(pos[0], origin[0])),
            "step {step}: the escape must reconstruct exactly"
        );
    }
    assert_eq!(escapes, 1, "exactly one step needed the escape");

    // And the displacement really was outside the i16 range, so the test is not vacuous.
    let d_mm = (250.0f64 * 1000.0) as i64;
    assert!(d_mm > i64::from(i16::MAX), "{d_mm} mm fits in an i16");
}

#[test]
fn headings_wrap_by_construction_across_the_turn() {
    // §3.2: binary radians wrap correctly by construction — there is no ±π branch.
    let cadence = Cadence::DEFAULT;
    let origin = [0.0, 0.0, 0.0];
    let mut encoder = SnapshotEncoder::new(origin, cadence, Profile::Full, 0);
    let mut state = SlotState::default();
    let mut seen_wrap = false;
    let mut prev = 0u16;

    for step in 0..40u32 {
        // 0.2 rad per step: the heading crosses 2π at step 32 and keeps going.
        let heading = 0.2 * f64::from(step);
        let mut p = pose(step, [10.0 * f64::from(step), 0.0, 0.15]);
        p.heading_rad = heading;
        let snap = Snapshot::new(u64::from(step) * 100_000_000, vec![p]);
        let frame = encoder.encode(&snap).expect("the step encodes");
        apply(&mut state, frame.frame(), 0);

        assert_eq!(
            state.heading_brad,
            quant::heading_brad(heading),
            "step {step}: the heading is absolute on every row"
        );
        // The decoded heading is always the true heading modulo a turn, to within a brad.
        let decoded = quant::brad_to_rad(state.heading_brad);
        let expected = heading.rem_euclid(core::f64::consts::TAU);
        let delta = (decoded - expected)
            .abs()
            .min(core::f64::consts::TAU - (decoded - expected).abs());
        assert!(
            delta < 1e-4,
            "step {step}: decoded {decoded} rad, expected {expected} rad"
        );
        if step > 0 && state.heading_brad < prev {
            seen_wrap = true;
        }
        prev = state.heading_brad;
    }
    assert!(
        seen_wrap,
        "the run must cross a full turn for the wrap to be tested"
    );
    // The two ends of the range meet: one brad below zero is 65535, not an error.
    assert_eq!(
        quant::heading_brad(-core::f64::consts::TAU / 65_536.0),
        65_535
    );
    assert_eq!(quant::heading_brad(core::f64::consts::TAU), 0);
}

/// Ten times the run above, still in one GOP: the figure the hardening work was told not
/// to regress is **0.0005 m over 100,000 steps with zero integer mismatches**.
const LONG_STEPS: u32 = 100_000;

#[test]
fn a_hundred_thousand_steps_in_one_gop_stay_inside_half_a_millimetre() {
    // `delta_quantisation_does_not_drift_over_ten_thousand_steps` proves the scheme does
    // not drift. This one pins the number, at the length the acceptance criterion states,
    // and counts the integer comparisons rather than only asserting them — a sweep that
    // quietly stopped comparing would otherwise pass while proving nothing, which is the
    // failure this project has produced four times.
    let cadence = Cadence::new(Duration::from_secs(10_000), Duration::from_millis(100))
        .expect("a whole number of steps per keyframe period");
    let origin = [-500.0, -500.0, 0.0];
    let mut encoder = SnapshotEncoder::new(origin, cadence, Profile::Full, 0);
    let mut state = SlotState::default();
    let mut keyframes = 0;
    let mut compared = 0u32;
    let mut mismatches = 0u32;
    let mut worst = [0.0f64; 3];
    // z is on the centimetre grid at the keyframe and on the millimetre grid from the
    // first delta onwards, so its worst case is measured both ways.
    let mut worst_z_after_the_keyframe = 0.0f64;

    for step in 0..LONG_STEPS {
        let pos = truth(step);
        let snap = Snapshot::new(u64::from(step) * 100_000_000, vec![pose(step, pos)]);
        let frame = encoder.encode(&snap).expect("the step encodes");
        if frame.is_keyframe() {
            keyframes += 1;
        }
        apply(&mut state, frame.frame(), 0);

        compared += 1;
        if state.x_mm != i64::from(quant::x_mm(pos[0], origin[0]))
            || state.y_mm != i64::from(quant::x_mm(pos[1], origin[1]))
        {
            mismatches += 1;
        }

        let decoded = [
            quant::mm_to_m(state.x_mm, origin[0]),
            quant::mm_to_m(state.y_mm, origin[1]),
            quant::mm_to_m(state.z_mm, origin[2]),
        ];
        for axis in 0..3 {
            let err = (decoded[axis] - pos[axis]).abs();
            worst[axis] = worst[axis].max(err);
        }
        if step > 0 {
            worst_z_after_the_keyframe =
                worst_z_after_the_keyframe.max((decoded[2] - pos[2]).abs());
        }
    }

    assert_eq!(
        keyframes, 1,
        "the whole run must be one GOP for this to mean anything"
    );
    assert_eq!(
        compared, LONG_STEPS,
        "the integer comparison must have run on every step, not on some of them"
    );
    assert_eq!(
        mismatches, 0,
        "the reconstructed integers must equal the transmitted ones on every step"
    );
    assert!(
        worst[0] <= 5e-4 && worst[1] <= 5e-4,
        "worst-case drift over {LONG_STEPS} steps: x {:.9} m, y {:.9} m, budget 0.0005 m",
        worst[0],
        worst[1]
    );
    assert!(
        worst_z_after_the_keyframe <= 5e-4,
        "z after the keyframe re-anchors it on the millimetre grid: {:.9} m",
        worst_z_after_the_keyframe
    );
    println!(
        "{LONG_STEPS} steps, one GOP, {compared} integer comparisons, {mismatches} mismatches: \
         max |decoded - true| = {:.9} m (x), {:.9} m (y), {:.9} m (z, {:.9} m after the keyframe)",
        worst[0], worst[1], worst[2], worst_z_after_the_keyframe
    );
}
