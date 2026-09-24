//! The signal block a page lights its heads from is the engine's own per-movement state.
//!
//! The owner saw lights that were "sometimes green, sometimes not". Two producers were at
//! fault: the engine's recorder wrote an empty signal block, so a recording played back
//! drew every lamp as "no data", and the fixture engine sent one row per controller
//! holding its first movement's state, so every head of a junction lit the same colour.
//! Both now carry one row per head group ([`v2xw_world::World::group_signals`]); these
//! tests hold that row to what the mobility engine's signal model tells the vehicles, on
//! the real Manhattan extract, over a whole cycle and after a seek forward and a rewind.

use std::collections::BTreeMap;
use std::path::Path;

use v2xw_core::ids::LaneId;
use v2xw_engine::Scenario;
use v2xw_engine::snapshot::{DEFAULT_KEYFRAME_PERIOD, SnapshotStream, snapshot_cadence};
use v2xw_mobility::FixedTimeSignals;
use v2xw_record::Profile;
use v2xw_record::wire::snapshot::KeyframeBody;
use v2xw_world::{SignalState, World};

fn rank(s: SignalState) -> u8 {
    match s {
        SignalState::Green => 6,
        SignalState::GreenYield => 5,
        SignalState::FlashingAmber => 4,
        SignalState::Amber => 3,
        SignalState::RedAmber => 2,
        SignalState::Red => 1,
        SignalState::Off => 0,
    }
}

/// The Manhattan world of `scenarios/manhattan-5min.yaml`, or `None` when the extract
/// (which is not in git) is not on this machine.
fn manhattan() -> Option<World> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut scenario =
        Scenario::load(root.join("scenarios/manhattan-5min.yaml")).expect("the scenario loads");
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut scenario.world.source {
        let full = root.join(&*path);
        if !full.exists() {
            return None;
        }
        *path = full.to_string_lossy().into_owned();
    }
    Some(v2xw_engine::wiring::build_world(&scenario).expect("the world builds"))
}

/// For every head group of every plan, at every `times` instant: the state the group
/// streams, and the state the engine shows the group's own movements (the most permissive
/// of them, as a head over an approach shows its through movement's green).
fn check(world: &World, times: &[f64]) -> (usize, usize) {
    let groups = world.group_signals();
    let engine = FixedTimeSignals::default();
    let mut approach_of: BTreeMap<LaneId, LaneId> = BTreeMap::new();
    for c in world.roads.connections() {
        if let Some(via) = c.via {
            approach_of.entry(via).or_insert(c.from_lane);
        }
    }
    let (mut samples, mut changes) = (0, 0);
    for plan in &world.signals {
        let mut ids: Vec<u16> = plan.heads.iter().map(|h| h.group).collect();
        ids.sort_unstable();
        ids.dedup();
        for group in ids {
            let wire = v2xw_world::signal_group_wire_id(plan.id, group);
            let streamed = groups
                .iter()
                .find(|g| g.wire_id == wire)
                .expect("every head group is streamed");
            let mut last = None;
            for &t in times {
                let expected = plan
                    .controlled
                    .iter()
                    .filter(|l| {
                        // A pedestrian head faces the crossing lane it controls.
                        plan.heads.iter().any(|h| {
                            h.lane == **l
                                && h.group == group
                                && h.kind == v2xw_world::SignalHeadKind::Pedestrian
                        }) || approach_of.get(l).is_some_and(|a| {
                            plan.heads.iter().any(|h| h.lane == *a && h.group == group)
                        })
                    })
                    .filter_map(|l| engine.state_for(plan, *l, t))
                    .max_by_key(|s| rank(*s))
                    .expect("the group controls a movement");
                let (shown, remaining) = streamed.at(t).expect("a state");
                assert_eq!(
                    shown, expected,
                    "plan {} group {group} at t = {t} s",
                    plan.id
                );
                assert!(remaining >= 0.0 && remaining <= plan.cycle_s);
                if last.is_some_and(|l| l != shown) {
                    changes += 1;
                }
                last = Some(shown);
                samples += 1;
            }
        }
    }
    (samples, changes)
}

#[test]
fn every_manhattan_head_shows_its_own_movements_state_over_a_cycle_and_after_seeks() {
    let Some(world) = manhattan() else {
        eprintln!("skipped: worlds/cache/manhattan.osm.xml is not on this machine");
        return;
    };
    assert!(world.signals.len() > 200, "{} plans", world.signals.len());
    let longest = world
        .signals
        .iter()
        .map(|p| p.cycle_s)
        .fold(0.0f64, f64::max);
    // A full cycle at the 0.1 s mobility step, then a seek forward to 1234.5 s and a
    // stretch after it, then a rewind to the start: the state is a function of time
    // alone, so where the playhead came from must not matter.
    let mut times: Vec<f64> = (0..=(longest * 10.0) as u64)
        .map(|k| k as f64 * 0.1)
        .collect();
    times.extend((0..300).map(|k| 1234.5 + k as f64 * 0.1));
    times.extend((0..100).map(|k| k as f64 * 0.1 + 0.05));
    let (samples, changes) = check(&world, &times);
    eprintln!("{samples} head-group samples, {changes} changes");
    assert!(changes > 1000, "only {changes} changes seen");
}

#[test]
fn a_recorded_keyframe_carries_one_row_per_head_group_in_its_state() {
    let world = v2xw_world::procedural::grid(
        &v2xw_world::procedural::GridParams::legacy().with_signals(true),
        &v2xw_world::ImportOptions::default(),
    )
    .expect("grid");
    let groups = world.group_signals();
    assert!(!groups.is_empty());
    let step = v2xw_core::time::Duration::from_millis(100);
    for t_ns in [0u64, 31_700_000_000] {
        // A stream opens with a keyframe, so a fresh stream per instant decodes as one.
        let mut s = SnapshotStream::new(
            &world.bbox,
            snapshot_cadence(step, DEFAULT_KEYFRAME_PERIOD),
            Profile::Full,
        )
        .with_signals(&world);
        let frame = s.encode(t_ns, &[]).expect("encodes");
        let body = KeyframeBody::decode(frame.body()).expect("a keyframe");
        assert_eq!(body.signals.len(), groups.len(), "at {t_ns} ns");
        let t_s = t_ns as f64 * 1e-9;
        for (row, g) in body.signals.iter().zip(&groups) {
            let (state, _) = g.at(t_s).expect("a state");
            assert_eq!(row.signal_id, g.wire_id);
            assert_eq!(
                row.phase,
                state.j2735_phase(),
                "group {} at {t_s} s",
                g.wire_id
            );
        }
    }
}
