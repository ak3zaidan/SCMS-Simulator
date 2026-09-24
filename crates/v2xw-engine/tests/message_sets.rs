//! The message sets beyond the BSM and the CAM, generated in a run by the stations that
//! send them in a deployment: SPaT and MAP from roadside units, SRM from emergency
//! vehicles and SSM in answer, and DENM from a vehicle that brakes hard.
//!
//! Each test checks the message against something that could disagree with it: a SPaT's
//! light against the signal plan the drivers obey, a count against the standard's rate, a
//! DENM against the braking in the ground truth, a refusal against the scenario that
//! should be refused.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use v2xw_core::ids::NodeId;
use v2xw_engine::{Engine, MemoryRecorder, RunReport, Scenario};
use v2xw_metrics::channels::{GtKinematicsView, NodeRxView, NodeTxView, RxFate, decode};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

fn connected(duration_s: f64) -> Scenario {
    let mut s = Scenario::load(scenarios().join("connected-intersections.yaml"))
        .expect("the shipped scenario loads");
    s.time.duration_s = duration_s;
    s.metrics = vec![];
    s
}

fn run(s: Scenario) -> (RunReport, MemoryRecorder, Engine) {
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    let report = engine.run(&mut recorder).expect("runs");
    (report, recorder, engine)
}

fn tx(recorder: &MemoryRecorder) -> Vec<NodeTxView> {
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "node.tx")
        .map(|(_, r)| decode(r).expect("node.tx decodes"))
        .collect()
}

fn by_type(txs: &[NodeTxView]) -> BTreeMap<String, Vec<&NodeTxView>> {
    let mut out: BTreeMap<String, Vec<&NodeTxView>> = BTreeMap::new();
    for t in txs {
        out.entry(t.msg_type.clone().unwrap_or_default())
            .or_default()
            .push(t);
    }
    out
}

/// Every unit broadcasts its junction's SPaT at 10 Hz and its MAP at 1 Hz, and only the
/// units do; the vehicles hear them and hand them to their applications.
#[test]
fn roadside_units_broadcast_spat_at_10_hz_and_map_at_1_hz() {
    let seconds = 5.0;
    let (report, recorder, engine) = run(connected(seconds));
    let rsus: BTreeSet<NodeId> = engine
        .phase2()
        .expect("roadside units make a Phase 2 run")
        .rsu_nodes()
        .iter()
        .copied()
        .collect();
    assert_eq!(rsus.len(), 4);
    assert_eq!(report.infra_encode_failures, 0);
    let txs = tx(&recorder);
    let kinds = by_type(&txs);
    let spat = kinds.get("spat").cloned().unwrap_or_default();
    let map = kinds.get("map").cloned().unwrap_or_default();
    eprintln!(
        "frames by type {:?}",
        kinds
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect::<Vec<_>>()
    );
    // Only the four units send them.
    assert!(spat.iter().all(|t| rsus.contains(&t.node)));
    assert!(map.iter().all(|t| rsus.contains(&t.node)));
    for unit in &rsus {
        let n_spat = spat.iter().filter(|t| t.node == *unit).count() as f64;
        let n_map = map.iter().filter(|t| t.node == *unit).count() as f64;
        // 10 Hz and 1 Hz over the run, give or take the first and the last period (the
        // unit steps at its own phase, and a frame due past the horizon is not sent).
        assert!(
            (n_spat - 10.0 * seconds).abs() <= 2.0,
            "unit {unit:?} sent {n_spat} SPaT in {seconds} s"
        );
        assert!(
            (n_map - seconds).abs() <= 1.0,
            "unit {unit:?} sent {n_map} MAP in {seconds} s"
        );
    }
    // A MAP is bigger than a SPaT, and both are real encodings with a signature around them.
    let size = |v: &[&NodeTxView]| {
        v.iter()
            .map(|t| t.payload_bytes.unwrap_or(0))
            .max()
            .unwrap_or(0)
    };
    assert!(
        size(&map) > size(&spat),
        "MAP {} B, SPaT {} B",
        size(&map),
        size(&spat)
    );
    assert!(size(&spat) > 10);
    // And the vehicles hear them.
    let delivered_spat = recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "node.rx")
        .map(|(_, r)| decode::<NodeRxView>(r).expect("node.rx decodes"))
        .filter(|v| v.outcome == RxFate::Delivered && v.msg_type.as_deref() == Some("spat"))
        .count();
    assert!(delivered_spat > 0, "no vehicle received a SPaT");
}

/// An emergency vehicle that heard a junction's MAP asks it for priority, and the unit
/// answers; no ordinary vehicle ever asks.
#[test]
fn emergency_vehicles_request_priority_and_the_units_answer() {
    // Half the fleet on blue lights, so a short run has several to follow.
    let mut s = connected(20.0);
    for class in s.actors.vehicles.classes.values_mut() {
        class.fraction = 0.5;
    }
    s.actors.vehicles.demand.rate_veh_per_h = Some(6_000.0);
    let (_, recorder, engine) = run(s);
    let rsus: BTreeSet<NodeId> = engine
        .phase2()
        .expect("units")
        .rsu_nodes()
        .iter()
        .copied()
        .collect();
    // Which nodes ride an emergency vehicle, from the ground truth.
    let mut emergency: BTreeSet<NodeId> = BTreeSet::new();
    for (_, r) in recorder.records() {
        if r.channel == "gt.kinematics" {
            let k: GtKinematicsView = decode(r).expect("decodes");
            if k.class.as_deref() == Some("emergency")
                && let Some(n) = k.node
            {
                emergency.insert(n);
            }
        }
    }
    let txs = tx(&recorder);
    let kinds = by_type(&txs);
    let srm = kinds.get("srm").cloned().unwrap_or_default();
    let ssm = kinds.get("ssm").cloned().unwrap_or_default();
    assert!(
        !emergency.is_empty(),
        "the fleet has no emergency vehicle to test with"
    );
    assert!(!srm.is_empty(), "no emergency vehicle asked for priority");
    assert!(
        srm.iter().all(|t| emergency.contains(&t.node)),
        "a vehicle without a priority entitlement sent an SRM"
    );
    assert!(!ssm.is_empty(), "no unit answered a signal request");
    assert!(ssm.iter().all(|t| rsus.contains(&t.node)));
    // One request a second at most from any one vehicle, by generation instant (the air
    // instant moves with channel access, the decision to send does not).
    let mut per_node: BTreeMap<NodeId, Vec<u64>> = BTreeMap::new();
    for t in &srm {
        per_node.entry(t.node).or_default().push(
            t.t_generated
                .expect("a node-generated frame carries its generation time"),
        );
    }
    for (node, times) in per_node {
        for w in times.windows(2) {
            assert!(
                w[1] - w[0] >= 999_000_000,
                "{node:?} sent two SRMs {} ms apart",
                (w[1] - w[0]) / 1_000_000
            );
        }
    }
}

/// A DENM is raised once per hard-braking episode — the rising edge of the vehicle's own
/// longitudinal deceleration through 0.4 g — and never otherwise. The episodes come from
/// the ground truth's own record of each vehicle's acceleration.
#[test]
fn a_denm_is_raised_by_hard_braking_and_by_nothing_else() {
    // A dense fleet, so queues form behind reds and somebody has to brake hard.
    let mut s = connected(40.0);
    s.actors.vehicles.demand.rate_veh_per_h = Some(20_000.0);
    s.net.layer = "gn-btp".to_string();
    s.security.envelope = "etsi103097".to_string();
    s.messages.sets = vec!["cam".into(), "denm".into(), "spat".into(), "map".into()];
    s.messages.codec_tier = "uper".to_string();
    let (_, recorder, _) = run(s);
    // Hard-braking rising edges per node, from gt.kinematics.
    let mut last: BTreeMap<NodeId, bool> = BTreeMap::new();
    let mut edges: BTreeMap<NodeId, u32> = BTreeMap::new();
    for (_, r) in recorder.records() {
        if r.channel != "gt.kinematics" {
            continue;
        }
        let k: GtKinematicsView = decode(r).expect("decodes");
        let Some(node) = k.node else { continue };
        let hard = k.acc_mps2.is_some_and(|a| a <= -3.92);
        let was = last.insert(node, hard).unwrap_or(false);
        if hard && !was {
            *edges.entry(node).or_insert(0) += 1;
        }
    }
    let txs = tx(&recorder);
    let denm = by_type(&txs).get("denm").cloned().unwrap_or_default();
    let senders: BTreeSet<NodeId> = denm.iter().map(|t| t.node).collect();
    // Every DENM comes from a vehicle that braked hard at least once.
    for n in &senders {
        assert!(
            edges.contains_key(n),
            "{n:?} sent a DENM and never braked hard"
        );
    }
    // And a vehicle that braked hard raised one: the episodes and the senders agree, up to
    // an episode in the run's last step (its DENM would be generated past the horizon).
    let braked: BTreeSet<NodeId> = edges.keys().copied().collect();
    let silent: Vec<&NodeId> = braked.difference(&senders).collect();
    assert!(
        silent.len() <= 1,
        "{} of {} hard-braking vehicles sent no DENM: {silent:?}",
        silent.len(),
        braked.len()
    );
    // Repetition: every 100 ms for 2 s, so about twenty frames per episode.
    let episodes: u32 = edges.values().sum();
    assert!(
        episodes > 0,
        "no vehicle braked hard, so nothing here was tested"
    );
    eprintln!(
        "hard-braking episodes {episodes} on {} vehicles; DENM frames {} from {} senders",
        braked.len(),
        denm.len(),
        senders.len()
    );
    if episodes > 0 {
        let per = denm.len() as f64 / f64::from(episodes);
        assert!(
            (10.0..=21.0).contains(&per),
            "{per} DENM frames per episode ({} over {episodes})",
            denm.len()
        );
    }
}

/// A SPaT says what the light is: for every junction, every signal group and a minute of
/// instants, the decoded SPaT's state is the most permissive state the mobility model's own
/// fixed-time controller shows any movement of that group (the rule the drivers obey), and
/// its `minEndTime` is when that group's light next changes, found by stepping the plan.
#[test]
fn a_spat_reports_the_light_the_drivers_see_and_when_it_changes() {
    use v2xw_engine::infra::{InfraFraming, IntersectionFeed, movement_phase};
    use v2xw_msg::j2735::spat;
    let s = connected(5.0);
    let world = v2xw_engine::wiring::build_world(&s).expect("the world builds");
    let wall = v2xw_core::time::WallClock::parse_rfc3339(&s.time.t0).expect("t0");
    let control = v2xw_mobility::FixedTimeSignals::default();
    let mut approach_of = BTreeMap::new();
    for c in world.roads.connections() {
        if let Some(via) = c.via {
            approach_of.entry(via).or_insert(c.from_lane);
        }
    }
    let rank = |st: v2xw_world::SignalState| {
        use v2xw_world::SignalState as S;
        match st {
            S::Green => 6,
            S::GreenYield => 5,
            S::FlashingAmber => 4,
            S::Amber => 3,
            S::RedAmber => 2,
            S::Red => 1,
            S::Off => 0,
        }
    };
    // The group state from the controller the mobility model runs, movement by movement.
    let group_state = |plan: &v2xw_world::SignalPlan, group: u16, t_s: f64| {
        plan.controlled
            .iter()
            .filter(|via| {
                // A pedestrian head faces the crosswalk lane it controls, which has no
                // approach (`SignalPlan::group_timelines`).
                plan.heads.iter().any(|h| {
                    h.lane == **via
                        && h.group == group
                        && h.kind == v2xw_world::SignalHeadKind::Pedestrian
                }) || approach_of
                    .get(via)
                    .is_some_and(|a| plan.heads.iter().any(|h| h.lane == *a && h.group == group))
            })
            .filter_map(|via| control.state_for(plan, *via, t_s))
            .max_by_key(|st| rank(*st))
            .unwrap_or(v2xw_world::SignalState::Off)
    };
    let origin: v2xw_core::geo::GeoOrigin = world.origin.into();
    let mut checked = 0;
    for (i, plan) in world.signals.iter().enumerate() {
        let feed = IntersectionFeed::build(&world, i, origin)
            .expect("every signalised junction has a MAP");
        let groups: BTreeSet<u16> = plan.heads.iter().map(|h| h.group).collect();
        for step in 0..60u64 {
            let t = step * 1_000_000_000 + 350_000_000;
            let t_s = (t as f64) * 1e-9;
            let bytes = feed
                .spat_bytes(t, wall, InfraFraming::J2735)
                .expect("encodes");
            let decoded = spat::decode_message_frame(&bytes).expect("decodes");
            let state = &decoded.intersections[0];
            assert_eq!(state.id.id, feed.intersection_id);
            let now_tenths = {
                let civil = wall.civil_at(t);
                f64::from(civil.minute) * 600.0 + f64::from(spat::d_second(wall, t)) / 100.0
            };
            for g in &groups {
                let sg = v2xw_engine::infra::signal_group_id(*g);
                let m = state
                    .states
                    .iter()
                    .find(|m| m.signal_group == sg)
                    .expect("every group is in the SPaT");
                let expect = group_state(plan, *g, t_s);
                assert_eq!(
                    m.events[0].event_state,
                    movement_phase(expect),
                    "junction {i} group {g} at {t_s} s"
                );
                // When it next changes, stepping the controller in 10 ms steps.
                let mut dt = 0.0;
                while dt < 2.0 * plan.cycle_s && group_state(plan, *g, t_s + dt) == expect {
                    dt += 0.01;
                }
                let timing = m.events[0]
                    .timing
                    .expect("a fixed-time SPaT states its end");
                let end_tenths = f64::from(timing.min_end_time);
                let said = (end_tenths - now_tenths).rem_euclid(36_000.0) / 10.0;
                assert!(
                    (said - dt).abs() <= 0.15,
                    "junction {i} group {g} at {t_s} s: SPaT says it changes in {said} s, the plan in {dt} s"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} group-instants checked");
}

/// The loader refuses what cannot be honoured, by name.
#[test]
fn what_a_scenario_cannot_have_is_refused_with_its_reason() {
    let refuse = |f: &dyn Fn(&mut Scenario), needle: &str| {
        let mut s = connected(5.0);
        f(&mut s);
        let err = s.validate().expect_err("refused").to_string();
        assert!(err.contains(needle), "expected '{needle}' in: {err}");
    };
    refuse(&|s| s.messages.sets.push("denm".into()), "gn-btp");
    refuse(&|s| s.messages.codec_tier = "uper".into(), "size-model");
    refuse(&|s| s.messages.sets.push("cpm".into()), "perception");
    refuse(
        &|s| {
            for r in &mut s.actors.rsus {
                r.roles = vec!["crl".into()];
            }
        },
        "role",
    );
    refuse(
        &|s| {
            s.actors.vehicles.classes.remove("emergency");
            if let Some(p) = s.actors.vehicles.classes.get_mut("passenger") {
                p.fraction = 1.0;
            }
        },
        "emergency",
    );
    // A unit with the role standing nowhere near a signal is refused at build.
    let mut s = connected(5.0);
    s.actors.rsus.truncate(1);
    s.actors.rsus[0].site = None;
    s.actors.rsus[0].position_m = Some([137.0, 40.0, 0.0]);
    let err = Engine::build(s, "")
        .expect_err("no junction in reach")
        .to_string();
    assert!(err.contains("signalised junction"), "{err}");
    // A unit whose hardware profile publishes no signing cost cannot sign a SPaT.
    let mut s = connected(5.0);
    s.actors.rsus[0].profile = Some("rsu/cohda-mk5-rsu".to_string());
    let err = Engine::build(s, "").expect_err("cannot sign").to_string();
    assert!(err.contains("signing cost"), "{err}");
}
