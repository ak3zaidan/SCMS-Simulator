//! INDEPENDENT VERIFIER INSTRUMENTATION — temporary, removed after the run.
//!
//! F2 re-derived without using the crate fixture's `parked_attacker`, and taken past
//! byte equality to the property that actually matters.
//!
//! Byte equality between one stripped stream and one live blind stream says the two
//! producers agree on that run. It does not say the blind stream is independent of the
//! ground truth, which is what a blind demonstration needs and what the surviving
//! all-zero row violated. So the test here is an INDISTINGUISHABILITY test:
//!
//!   build many runs that differ ONLY in §5.2 ground truth — lane, acceleration, the
//!   `ST_ATTACKER` bit, spawn and despawn causes, and the existence, position and class
//!   of unequipped actors — and assert that every one of them produces the SAME blind
//!   bytes, from both producers.
//!
//! If the pattern of emitted rows carried one bit about a withheld field, two runs that
//! differ only in that field would produce different blind streams, and this fails. The
//! row pattern is also compared on its own (`shape_of`), so a future change that made the
//! bytes differ for a legitimate reason could not hide a leak behind it.
//!
//! Non-vacuity is asserted explicitly: the same variants must produce DIFFERENT `full`
//! streams, and the parked actor must genuinely get moved rows in the full stream at the
//! steps where its hidden fields change.

use std::collections::BTreeMap;

use v2xw_core::{ActorId, Duration, LaneId, NodeId, SimTime};
use v2xw_record::encoder::{ActorPose, Cadence, SignalState, Snapshot, SnapshotEncoder};
use v2xw_record::fixture::scratch_dir;
use v2xw_record::profile::{NodeProfileStripper, Profile};
use v2xw_record::wire::snapshot::{
    DeltaBody, ST_ATTACKER, ST_EQUIPPED, ST_TRANSMITTING, KeyframeBody,
};
use v2xw_record::wire::{Frame, MsgType};
use v2xw_record::{Reader, RecordingOptions, RecordingWriter};

const ORIGIN: [f64; 3] = [-500.0, -500.0, 0.0];
const STEPS: u32 = 45;

/// Everything about a run that the `NODE-only` profile is supposed to withhold.
#[derive(Debug, Clone, PartialEq)]
struct GroundTruth {
    /// Step at which the parked actor changes lane, per parked actor.
    lane_change_at: Vec<u32>,
    /// Steps at which the parked actor's `ST_ATTACKER` bit toggles.
    attacker_toggles: Vec<u32>,
    /// A per-actor acceleration offset (accel is blanked to zero).
    accel_bias: f64,
    /// Lane numbering base for the moving actors.
    lane_base: u32,
    /// Spawn / despawn causes.
    causes: (u16, u16),
    /// Whether the unequipped pedestrian in the top slot exists at all, and where.
    ghost: Option<(u32, u32, f64, u8)>,
}

impl GroundTruth {
    fn baseline() -> Self {
        GroundTruth {
            lane_change_at: vec![u32::MAX, u32::MAX],
            attacker_toggles: vec![],
            accel_bias: 0.0,
            lane_base: 40,
            causes: (0, 0),
            ghost: None,
        }
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n)) as u32
    }
}

/// Slots: 0..=2 moving equipped, 3 unequipped pedestrian (middle slot), 4 and 5 parked
/// equipped (5 is the attacker), 6 the "ghost" — an unequipped actor in the TOP slot,
/// which is the one that could move the keyframe's `actor_count`.
const N_MOVING: u32 = 3;
const PED_SLOT: u32 = 3;
const PARKED: [u32; 2] = [4, 5];
const GHOST_SLOT: u32 = 6;

fn snapshots(gt: &GroundTruth) -> Vec<Snapshot> {
    let cadence = Cadence::DEFAULT;
    let step_ns = cadence.mobility_step.as_nanos();
    let mut out = Vec::with_capacity(STEPS as usize);
    for k in 0..STEPS {
        let t: SimTime = u64::from(k) * step_ns;
        let secs = (k as f64) * (step_ns as f64) / 1e9;
        let mut actors = Vec::new();
        for i in 0..N_MOVING {
            let speed = 11.0 + f64::from(i) / 3.0;
            actors.push(ActorPose {
                slot: i,
                actor: ActorId::new(i),
                node: Some(NodeId::new(i)),
                pos_m: [
                    -470.0 + f64::from(i) * 6.0 + speed * secs,
                    2.25 * v2xw_core::math::sin(0.4 * secs + f64::from(i)),
                    0.17 + f64::from(i % 3) * 0.01,
                ],
                heading_rad: 0.03 * secs * f64::from(i + 1),
                speed_mps: speed,
                // GROUND TRUTH: varies between variants.
                accel_mps2: gt.accel_bias + 0.25 * v2xw_core::math::cos(0.3 * secs),
                // GROUND TRUTH: varies between variants.
                lane: Some(LaneId::new(gt.lane_base + (k / 7) % 4 + i)),
                class_idx: (i % 3) as u8,
                state: ST_EQUIPPED | ST_TRANSMITTING,
                verified_neighbors: ((k / 9) % 5) as u8,
            });
        }
        // An unequipped pedestrian in a middle slot: present in the full stream, absent
        // from a blind one, so the blind keyframe must keep the slot empty.
        actors.push(ActorPose {
            slot: PED_SLOT,
            actor: ActorId::new(PED_SLOT),
            node: None,
            pos_m: [-100.0 + 1.3 * secs, 4.0, 0.0],
            heading_rad: 1.0,
            speed_mps: 1.3,
            accel_mps2: gt.accel_bias,
            lane: Some(LaneId::new(gt.lane_base + 90)),
            class_idx: 4,
            state: 0,
            verified_neighbors: 0,
        });
        // Two parked equipped actors, quantised-constant by construction: whole
        // millimetres about the origin, a whole centimetre of z, heading and speed zero.
        for (n, slot) in PARKED.iter().enumerate() {
            let is_attacker = gt
                .attacker_toggles
                .iter()
                .filter(|s| k >= **s)
                .count()
                .is_multiple_of(2)
                == false
                && n == 1;
            let changed = gt.lane_change_at.get(n).is_some_and(|s| k >= *s);
            actors.push(ActorPose {
                slot: *slot,
                actor: ActorId::new(*slot),
                node: Some(NodeId::new(*slot)),
                pos_m: [120.0 + (n as f64) * 5.0, -60.0, 0.25],
                heading_rad: 0.0,
                speed_mps: 0.0,
                accel_mps2: 0.0,
                // GROUND TRUTH.
                lane: Some(LaneId::new(gt.lane_base + 70 + u32::from(changed))),
                class_idx: 1,
                // GROUND TRUTH: the attacker bit. The `ST_TRANSMITTING` bit and
                // `verified_neighbors` beside it are NODE-visible and identical in every
                // variant: they are here so that over-dropping — a predicate that threw
                // away any all-zero row — would be caught as well as under-dropping.
                state: ST_EQUIPPED
                    | if n == 0 && (k / 8).is_multiple_of(2) { 0 } else { ST_TRANSMITTING }
                    | if is_attacker { ST_ATTACKER } else { 0 },
                verified_neighbors: if n == 0 { ((k / 6) % 4) as u8 } else { 3 },
            });
        }
        // GROUND TRUTH: an unequipped actor in the TOP slot, which in a full stream
        // raises `actor_count` and in a blind one must leave no trace at all.
        if let Some((from, to, x, class)) = gt.ghost {
            if (from..to).contains(&k) {
                actors.push(ActorPose {
                    slot: GHOST_SLOT,
                    actor: ActorId::new(GHOST_SLOT),
                    node: None,
                    pos_m: [x, 9.0, 0.0],
                    heading_rad: 2.0,
                    speed_mps: 0.9,
                    accel_mps2: gt.accel_bias,
                    lane: Some(LaneId::new(gt.lane_base + 95)),
                    class_idx: class,
                    state: 0,
                    verified_neighbors: 0,
                });
            }
        }
        let signals = (0..2u32)
            .map(|s| SignalState {
                signal: v2xw_core::SignalId::new(s),
                phase: (((k / 5) + s) % 10) as u8,
                time_to_change: Some(Duration::from_millis(u64::from(100 - (k % 60)) * 100)),
            })
            .collect();
        let mut spawn_causes = BTreeMap::new();
        let mut despawn_causes = BTreeMap::new();
        spawn_causes.insert(GHOST_SLOT, gt.causes.0);
        despawn_causes.insert(GHOST_SLOT, gt.causes.1);
        out.push(Snapshot {
            sim_time: t,
            actors,
            signals,
            spawn_causes,
            despawn_causes,
        });
    }
    out
}

fn encode(gt: &GroundTruth, profile: Profile) -> Vec<Frame> {
    let mut enc = SnapshotEncoder::new(ORIGIN, Cadence::DEFAULT, profile, 0);
    snapshots(gt)
        .iter()
        .map(|s| enc.encode(s).expect("encodes").into_frame())
        .collect()
}

fn strip_all(full: &[Frame]) -> Vec<Frame> {
    let mut s = NodeProfileStripper::new();
    full.iter()
        .filter_map(|f| s.strip(f).expect("strips"))
        .collect()
}

/// §7.2 compares canonical frames: the header with `flags &= CANONICAL_FLAG_MASK` and
/// the whole body. `FLAG_RESYNC` is a transport bit the sender sets and the recorder
/// clears, so it is masked here exactly as the guarantee masks it.
fn bytes_of(fs: &[Frame]) -> Vec<Vec<u8>> {
    fs.iter().map(|f| f.canonical().as_bytes().to_vec()).collect()
}

/// The PATTERN of a stream: which rows exist, with no field values at all. This is the
/// channel the surviving all-zero row leaked through.
#[derive(Debug, PartialEq, Eq, Clone)]
struct Shape {
    kind: Option<MsgType>,
    moved: Vec<u32>,
    spawns: Vec<u32>,
    despawns: Vec<u32>,
    abs: usize,
    lanes: usize,
    actor_count: usize,
    occupied: Vec<u32>,
}

fn shape_of(f: &Frame) -> Shape {
    let h = f.header().expect("header");
    let kind = h.kind();
    let mut s = Shape {
        kind,
        moved: vec![],
        spawns: vec![],
        despawns: vec![],
        abs: 0,
        lanes: 0,
        actor_count: 0,
        occupied: vec![],
    };
    match kind {
        Some(MsgType::Delta) => {
            let d = DeltaBody::decode(f.body()).expect("delta");
            s.moved = d.moved.iter().map(|m| m.slot).collect();
            s.spawns = d.spawns.iter().map(|x| x.slot).collect();
            s.despawns = d.despawns.iter().map(|x| x.slot).collect();
            s.abs = d.abs.len();
            s.lanes = d.lanes.len();
        }
        Some(MsgType::Keyframe) => {
            let k = KeyframeBody::decode(f.body()).expect("keyframe");
            s.actor_count = k.actors.len();
            s.occupied = k
                .actors
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_occupied())
                .map(|(i, _)| i as u32)
                .collect();
        }
        _ => {}
    }
    s
}

fn shapes(fs: &[Frame]) -> Vec<Shape> {
    fs.iter().map(shape_of).collect()
}

fn variants() -> Vec<GroundTruth> {
    let mut v = vec![GroundTruth::baseline()];
    // Hand-chosen corners.
    v.push(GroundTruth {
        lane_change_at: vec![u32::MAX, 3],
        ..GroundTruth::baseline()
    });
    v.push(GroundTruth {
        attacker_toggles: vec![6],
        ..GroundTruth::baseline()
    });
    v.push(GroundTruth {
        lane_change_at: vec![11, 3],
        attacker_toggles: vec![6, 14, 22],
        ..GroundTruth::baseline()
    });
    v.push(GroundTruth {
        ghost: Some((10, 30, -200.0, 5)),
        causes: (7, 9),
        ..GroundTruth::baseline()
    });
    v.push(GroundTruth {
        lane_change_at: vec![1, 1],
        attacker_toggles: vec![0],
        accel_bias: 3.75,
        lane_base: 900,
        causes: (u16::MAX - 1, 12),
        ghost: Some((2, 44, 350.0, 2)),
    });
    // …and a randomised sweep over the same dimensions.
    let mut rng = Lcg(0xDEAD_BEEF_0BAD_F00D);
    for _ in 0..34 {
        let ghost = if rng.below(2) == 0 {
            None
        } else {
            let a = rng.below(STEPS - 2);
            Some((
                a,
                a + 1 + rng.below(STEPS - a - 1),
                -300.0 + f64::from(rng.below(600)),
                (rng.below(6)) as u8,
            ))
        };
        let mut toggles: Vec<u32> = (0..rng.below(4)).map(|_| rng.below(STEPS)).collect();
        toggles.sort_unstable();
        toggles.dedup();
        v.push(GroundTruth {
            lane_change_at: vec![rng.below(STEPS + 5), rng.below(STEPS + 5)],
            attacker_toggles: toggles,
            accel_bias: f64::from(rng.below(1000)) / 97.0,
            lane_base: 40 + rng.below(500),
            causes: (rng.below(60000) as u16, rng.below(60000) as u16),
            ghost,
        });
    }
    v
}

#[test]
fn the_blind_stream_is_the_same_bytes_whatever_the_ground_truth_does() {
    let vs = variants();
    let base_live = encode(&vs[0], Profile::NodeOnly);
    let base_stripped = strip_all(&encode(&vs[0], Profile::Full));
    assert_eq!(
        bytes_of(&base_live),
        bytes_of(&base_stripped),
        "V5 fails on the baseline"
    );
    let want_bytes = bytes_of(&base_live);
    let want_shape = shapes(&base_live);

    let mut full_shapes_differing = 0usize;
    let base_full_shapes = shapes(&encode(&vs[0], Profile::Full));
    for (i, gt) in vs.iter().enumerate() {
        let live = encode(gt, Profile::NodeOnly);
        let full = encode(gt, Profile::Full);
        let stripped = strip_all(&full);
        assert_eq!(
            bytes_of(&live),
            want_bytes,
            "variant {i}: the LIVE blind stream depends on ground truth"
        );
        assert_eq!(
            bytes_of(&stripped),
            want_bytes,
            "variant {i}: the STRIPPED blind stream depends on ground truth (V5/V1)"
        );
        assert_eq!(
            shapes(&live),
            want_shape,
            "variant {i}: the live blind ROW PATTERN depends on ground truth"
        );
        assert_eq!(
            shapes(&stripped),
            want_shape,
            "variant {i}: the stripped blind ROW PATTERN depends on ground truth"
        );
        if shapes(&full) != base_full_shapes {
            full_shapes_differing += 1;
        }
    }
    println!(
        "INDEP-BLIND {} ground-truth variants, all identical blind bytes ({} frames, {} bytes); \
         {} of them have a DIFFERENT full-stream row pattern",
        vs.len(),
        want_bytes.len(),
        want_bytes.iter().map(Vec::len).sum::<usize>(),
        full_shapes_differing
    );
    assert!(
        full_shapes_differing >= vs.len() - 6,
        "the variants barely move the full stream ({full_shapes_differing}); the test proves little"
    );
}

#[test]
fn the_full_stream_really_does_emit_the_rows_the_blind_one_must_not() {
    // Non-vacuity, stated as a number: the parked attacker gets moved rows in the full
    // stream exactly at the steps where its hidden fields change, and none in either
    // blind stream, ever.
    let gt = GroundTruth {
        lane_change_at: vec![u32::MAX, 3],
        attacker_toggles: vec![6, 14],
        ..GroundTruth::baseline()
    };
    let full = encode(&gt, Profile::Full);
    let live = encode(&gt, Profile::NodeOnly);
    let stripped = strip_all(&full);
    let count = |fs: &[Frame], slot: u32| -> usize {
        fs.iter()
            .filter(|f| f.header().unwrap().kind() == Some(MsgType::Delta))
            .map(|f| {
                DeltaBody::decode(f.body())
                    .unwrap()
                    .moved
                    .iter()
                    .filter(|m| m.slot == slot)
                    .count()
            })
            .sum()
    };
    let in_full = count(&full, PARKED[1]);
    let in_live = count(&live, PARKED[1]);
    let in_strip = count(&stripped, PARKED[1]);
    println!(
        "INDEP-BLIND parked attacker slot {}: full emits {in_full} moved rows, \
         live blind {in_live}, stripped blind {in_strip}",
        PARKED[1]
    );
    assert_eq!(in_full, 3, "the fixture must exercise the divergence");
    assert_eq!(in_live, 0);
    assert_eq!(in_strip, 0);
    // The other parked actor changes only NODE-visible state, so both blind producers
    // must keep emitting rows for it: a predicate that dropped every all-zero row would
    // pass the assertions above and fail these.
    let visible_full = count(&full, PARKED[0]);
    let visible_live = count(&live, PARKED[0]);
    let visible_strip = count(&stripped, PARKED[0]);
    println!(
        "INDEP-BLIND parked NODE-visible slot {}: full {visible_full}, live blind \
         {visible_live}, stripped blind {visible_strip}",
        PARKED[0]
    );
    assert!(visible_full >= 8, "the visible-change fixture is too quiet");
    assert_eq!(visible_live, visible_full);
    assert_eq!(visible_strip, visible_full);
    // The unequipped pedestrian and the ghost never appear in a blind stream at all.
    for slot in [PED_SLOT, GHOST_SLOT] {
        assert_eq!(count(&live, slot), 0);
        assert_eq!(count(&stripped, slot), 0);
    }
}

#[test]
fn v5_holds_through_the_container_with_headers_compared() {
    // The same claim, but through MCAP: write the full recording, replay it, strip it,
    // write the blind recording, replay that, and compare every frame BYTE FOR BYTE —
    // header included — with a live blind run.
    let dir = scratch_dir("indep-v5").expect("scratch");
    let gt = GroundTruth {
        lane_change_at: vec![11, 3],
        attacker_toggles: vec![6, 14, 22],
        accel_bias: 1.5,
        lane_base: 77,
        causes: (3, 4),
        ghost: Some((9, 31, -120.0, 5)),
    };
    let full_path = dir.join("full.mcap");
    let blind_path = dir.join("node.mcap");
    let full = encode(&gt, Profile::Full);
    {
        let mut w = RecordingWriter::create(&full_path, RecordingOptions::default())
            .expect("full writer");
        w.write_manifest(r#"{"schema":"v2xw/manifest/1","run":"indep"}"#)
            .expect("manifest");
        for f in &full {
            w.write_frame(f).expect("write");
        }
        w.finish().expect("finish");
    }
    let mut r = Reader::open(&full_path).expect("opens");
    let replayed: Vec<Frame> = r.replay().expect("replays").into_iter().map(|f| f.frame).collect();
    assert_eq!(bytes_of(&replayed), bytes_of(&full), "byte identity broke");

    let stripped = strip_all(&replayed);
    {
        let mut w = RecordingWriter::create(
            &blind_path,
            RecordingOptions {
                profile: Profile::NodeOnly,
                ..Default::default()
            },
        )
        .expect("blind writer");
        w.write_manifest(r#"{"schema":"v2xw/manifest/1","profile":"node"}"#)
            .expect("manifest");
        for f in &stripped {
            w.write_frame(f).expect("write");
        }
        w.finish().expect("finish");
    }
    let mut br = Reader::open(&blind_path).expect("opens");
    br.verify().expect("the blind recording verifies");
    let blind_replayed: Vec<Frame> = br
        .replay()
        .expect("replays")
        .into_iter()
        .map(|f| f.frame)
        .collect();
    let live = encode(&gt, Profile::NodeOnly);
    assert_eq!(blind_replayed.len(), live.len(), "frame count differs");
    for (i, (a, b)) in blind_replayed.iter().zip(live.iter()).enumerate() {
        assert_eq!(
            a.canonical().as_bytes(),
            b.canonical().as_bytes(),
            "canonical frame {i} ({:?}) differs between the stripped recording and a live node run",
            a.header().unwrap().kind()
        );
    }
    println!(
        "INDEP-BLIND V5 through MCAP: {} frames byte-identical including headers",
        live.len()
    );
}
