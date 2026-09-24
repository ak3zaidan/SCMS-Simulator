//! Every event kind on the scenario timeline acts, and says what it did.
//!
//! Before this, `outage` and `weather.front` were the only kinds that changed anything; a
//! `closure`, a `demand.multiplier`, a `param.change` and an `attack.wave` fired, were
//! counted, and did nothing. Each test here runs a scenario with the event against the same
//! scenario without it, so an event that stopped reaching the run fails rather than passes.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use v2xw_engine::scenario::schema::{TimelineItem, TimelineKind};
use v2xw_engine::{Engine, MemoryRecorder, Scenario};

/// A signalised procedural grid with steady Poisson demand and no radios: these tests are
/// about the road and the fleet, and an unequipped fleet runs in a fraction of the time.
fn grid(duration_s: f64, rate: f64) -> Scenario {
    let mut s = Scenario::minimal();
    s.actors.vehicles.equipped_fraction = 0.0;
    s.world.source = v2xw_world::WorldSourceSpec::procedural(
        "world/source/procedural-grid",
        json!({
            "cols": 3, "rows": 3, "block_x_m": 150.0, "block_y_m": 150.0,
            "lanes_per_direction": 2, "speed_limit_mps": 13.89, "signalised": true,
            "corner_radius_m": 4.5
        }),
    );
    s.time.duration_s = duration_s;
    s.actors.vehicles.demand.kind = "mobility/demand/poisson".to_string();
    s.actors.vehicles.demand.rate_veh_per_h = Some(rate);
    s
}

fn item(t: f64, until: Option<f64>, kind: TimelineKind, params: Value) -> TimelineItem {
    TimelineItem {
        t,
        until,
        kind,
        params: params
            .as_object()
            .expect("an object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

struct Run {
    /// `(t, actor, lane)` for every published state, in emission order.
    states: Vec<(u64, u32, Option<u32>)>,
    /// Every `scenario.event` record.
    events: Vec<Value>,
    spawned: u64,
    despawns: BTreeMap<String, u64>,
}

fn run(s: Scenario) -> Run {
    let mut engine = Engine::build(s, "").expect("the scenario builds");
    let mut rec = MemoryRecorder::new();
    let report = engine.run(&mut rec).expect("the run completes");
    let mut states = Vec::new();
    let mut events = Vec::new();
    for (t, r) in rec.records() {
        match r.channel {
            "gt.kinematics" => {
                let v: Value = serde_json::from_slice(&r.json).expect("json");
                states.push((
                    *t,
                    v["actor"].as_u64().expect("actor") as u32,
                    v["lane"].as_u64().map(|l| l as u32),
                ));
            }
            "scenario.event" => events.push(serde_json::from_slice(&r.json).expect("json")),
            _ => {}
        }
    }
    Run {
        states,
        events,
        spawned: report.actors_spawned,
        despawns: report.despawn_causes.clone(),
    }
}

/// How many times a vehicle moved onto one of `lanes` after `after_ns` — from a lane that
/// was not one of them. A vehicle that was already on a closed lane is not counted: it
/// finishes that lane, which is the documented behaviour.
fn entries(states: &[(u64, u32, Option<u32>)], lanes: &BTreeSet<u32>, after_ns: u64) -> usize {
    let mut last: BTreeMap<u32, Option<u32>> = BTreeMap::new();
    let mut n = 0;
    for (t, actor, lane) in states {
        let before = last.insert(*actor, *lane).flatten();
        if *t > after_ns
            && lane.is_some_and(|l| lanes.contains(&l))
            && before.is_some_and(|b| !lanes.contains(&b))
        {
            n += 1;
        }
    }
    n
}

#[test]
fn a_closure_keeps_traffic_off_the_closed_road_and_vehicles_route_around_it() {
    const CLOSE_AT: f64 = 10.0;
    let base = grid(90.0, 3600.0);
    let world = v2xw_engine::wiring::build_world(&base).expect("world");
    let baseline = run(base.clone());

    // The busiest street edge after the closure instant, in the run without the closure.
    let lane_edge: BTreeMap<u32, u32> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == v2xw_world::model::LaneKind::Driving)
        .map(|l| (l.id.index(), l.edge.index()))
        .collect();
    let mut traffic: BTreeMap<u32, usize> = BTreeMap::new();
    for edge in lane_edge.values().collect::<BTreeSet<_>>() {
        let lanes: BTreeSet<u32> = lane_edge
            .iter()
            .filter(|(_, e)| *e == edge)
            .map(|(l, _)| *l)
            .collect();
        traffic.insert(
            *edge,
            entries(&baseline.states, &lanes, (CLOSE_AT * 1e9) as u64),
        );
    }
    let (&edge, &before) = traffic
        .iter()
        .max_by_key(|(e, n)| (**n, std::cmp::Reverse(**e)))
        .expect("an edge");
    assert!(
        before >= 5,
        "the chosen street carries traffic without the closure ({before} entries), so the \
         check below can fail"
    );

    let mut closed = base.clone();
    closed.events = vec![item(
        CLOSE_AT,
        None,
        TimelineKind::Closure,
        json!({"target": format!("edge:{edge}")}),
    )];
    let with = run(closed);
    let lanes: BTreeSet<u32> = lane_edge
        .iter()
        .filter(|(_, e)| **e == edge)
        .map(|(l, _)| *l)
        .collect();
    assert_eq!(
        entries(&with.states, &lanes, (CLOSE_AT * 1e9) as u64),
        0,
        "no vehicle drove onto the closed street after it closed (it had {before} without \
         the closure)"
    );
    // Traffic kept moving: the closure did not empty the network.
    let moving_after = with
        .states
        .iter()
        .filter(|(t, ..)| *t > ((CLOSE_AT + 10.0) * 1e9) as u64)
        .count();
    assert!(
        moving_after > 1000,
        "{moving_after} vehicle-steps after the closure"
    );
    // Most vehicles that were headed through it re-planned rather than leaving the run.
    let blocked = with.despawns.get("RouteBlocked").copied().unwrap_or(0);
    assert!(
        (blocked as usize) < before,
        "{blocked} vehicles left at the barrier, against {before} that used the street"
    );
    // And it said what it did.
    let record = with.events.first().expect("a scenario.event record");
    assert_eq!(record["kind"], "closure");
    assert_eq!(record["phase"], "start");
    assert_eq!(record["t"], json!((CLOSE_AT * 1e9) as u64));
    let recorded: BTreeSet<u32> = record["lanes"]
        .as_array()
        .expect("lanes")
        .iter()
        .map(|v| v.as_u64().expect("lane") as u32)
        .collect();
    assert_eq!(recorded, lanes, "the record names the lanes it closed");
    println!(
        "closure of edge {edge}: {before} entries without it, 0 with; {blocked} left at the \
         barrier; effect: {}",
        record["effect"]
    );
}

/// The street edge with the most vehicles driving onto it inside `[from_s, to_s)`, in the run
/// of `base`, with its vehicle lanes and that count.
fn busiest_edge(base: &Scenario, from_s: f64, to_s: f64) -> (u32, BTreeSet<u32>, usize) {
    let world = v2xw_engine::wiring::build_world(base).expect("world");
    let baseline = run(base.clone());
    let window: Vec<_> = baseline
        .states
        .iter()
        .filter(|(t, ..)| ((from_s * 1e9) as u64..(to_s * 1e9) as u64).contains(t))
        .cloned()
        .collect();
    let mut best: Option<(u32, BTreeSet<u32>, usize)> = None;
    for edge in world.roads.edges().iter().filter(|e| !e.is_internal()) {
        let lanes: BTreeSet<u32> = edge
            .lanes
            .iter()
            .filter(|l| world.lane(**l).kind == v2xw_world::model::LaneKind::Driving)
            .map(|l| l.index())
            .collect();
        let n = entries(&window, &lanes, (from_s * 1e9) as u64);
        if best.as_ref().is_none_or(|b| n > b.2) {
            best = Some((edge.id.index(), lanes, n));
        }
    }
    best.expect("a street")
}

#[test]
fn a_closure_with_an_until_reopens_the_road() {
    let base = grid(110.0, 7200.0);
    let (edge, lanes, before) = busiest_edge(&base, 10.0, 60.0);
    assert!(
        before >= 3,
        "the street is used while it would be closed: {before}"
    );
    let mut s = base.clone();
    s.events = vec![item(
        10.0,
        Some(60.0),
        TimelineKind::Closure,
        json!({"target": format!("edge:{edge}")}),
    )];
    let r = run(s);
    assert_eq!(
        r.events
            .iter()
            .map(|e| e["phase"].clone())
            .collect::<Vec<_>>(),
        vec![json!("start"), json!("end")]
    );
    // Closed between 10 s and 60 s: nobody drives onto it...
    let window = |from: f64, to: f64| -> Vec<(u64, u32, Option<u32>)> {
        r.states
            .iter()
            .filter(|(t, ..)| ((from * 1e9) as u64..(to * 1e9) as u64).contains(t))
            .cloned()
            .collect()
    };
    assert_eq!(
        entries(&window(10.0, 60.0), &lanes, 10_000_000_000),
        0,
        "{before} vehicles drove onto it in that window without the closure"
    );
    // ...and after it reopens, traffic uses it again.
    let after = entries(&window(60.0, 110.0), &lanes, 60_000_000_000);
    assert!(after > 0, "the reopened street carries traffic again");
}

#[test]
fn a_closure_target_that_names_nothing_is_refused_when_the_run_is_built() {
    let mut s = grid(10.0, 600.0);
    s.events = vec![item(
        2.0,
        None,
        TimelineKind::Closure,
        json!({"target": "edge:999999"}),
    )];
    let err = Engine::build(s.clone(), "").expect_err("a closure of nothing");
    assert!(err.to_string().contains("events[0].target"), "{err}");
    s.events[0]
        .params
        .insert("target".into(), json!("junction 4"));
    let err = Engine::build(s, "").expect_err("an unreadable target");
    assert!(err.to_string().contains("events[0].target"), "{err}");
}

#[test]
fn a_demand_multiplier_scales_the_arrivals_while_it_is_in_force() {
    let base = grid(120.0, 1200.0);
    let plain = run(base.clone());
    let mut surged = base.clone();
    surged.events = vec![item(
        0.0,
        Some(120.0),
        TimelineKind::DemandMultiplier,
        json!({"value": 3.0}),
    )];
    let more = run(surged);
    // 40 vehicles expected in two minutes at 1,200 veh/h, 120 at three times that. The
    // insertion queue refuses some on a busy grid, so the band is wide; the point is the
    // factor.
    let ratio = more.spawned as f64 / plain.spawned.max(1) as f64;
    assert!(
        (2.0..=3.6).contains(&ratio),
        "{} spawned with the multiplier against {} without: {ratio:.2}x",
        more.spawned,
        plain.spawned
    );
    assert_eq!(more.events[0]["multiplier"], json!(3.0));
    assert_eq!(more.events[1]["phase"], "end");
}

#[test]
fn a_param_change_of_the_arrival_rate_takes_effect_at_its_instant() {
    let base = grid(120.0, 600.0);
    let plain = run(base.clone());
    let mut s = base.clone();
    s.events = vec![item(
        0.0,
        None,
        TimelineKind::ParamChange,
        json!({"path": "actors.vehicles.demand.rate_veh_per_h", "value": 2400.0}),
    )];
    let faster = run(s);
    let ratio = faster.spawned as f64 / plain.spawned.max(1) as f64;
    assert!(
        (2.5..=5.0).contains(&ratio),
        "{} against {}: {ratio:.2}x for a fourfold rate",
        faster.spawned,
        plain.spawned
    );
    assert!(
        faster.events[0]["effect"]
            .as_str()
            .expect("effect")
            .contains("2400"),
        "{}",
        faster.events[0]
    );
}

#[test]
fn a_param_change_of_the_equipped_share_reaches_vehicles_that_enter_after_it() {
    let base = grid(90.0, 2400.0);
    let mut s = base.clone();
    s.actors.vehicles.equipped_fraction = 1.0;
    s.events = vec![item(
        30.0,
        None,
        TimelineKind::ParamChange,
        json!({"path": "actors.vehicles.equipped_fraction", "value": 0.0}),
    )];
    let mut engine = Engine::build(s, "").expect("builds");
    let mut rec = MemoryRecorder::new();
    engine.run(&mut rec).expect("runs");
    // Spawns before 30 s carry nodes; spawns after carry none.
    let mut equipped_before = 0;
    let mut equipped_after = 0;
    let mut spawned_after = 0;
    let mut first_seen: BTreeMap<u64, (u64, bool)> = BTreeMap::new();
    for (t, r) in rec.records() {
        if r.channel != "gt.kinematics" {
            continue;
        }
        let v: Value = serde_json::from_slice(&r.json).expect("json");
        let actor = v["actor"].as_u64().expect("actor");
        first_seen
            .entry(actor)
            .or_insert((*t, v.get("node").is_some_and(|n| !n.is_null())));
    }
    for (t, has_node) in first_seen.values() {
        if *t < 30_000_000_000 {
            equipped_before += usize::from(*has_node);
        } else {
            spawned_after += 1;
            equipped_after += usize::from(*has_node);
        }
    }
    assert!(
        equipped_before > 5,
        "{equipped_before} equipped before the change"
    );
    assert!(
        spawned_after > 5,
        "{spawned_after} vehicles entered after the change"
    );
    assert_eq!(equipped_after, 0, "none of them carries an OBU");
}

#[test]
fn a_param_change_the_run_cannot_honour_is_refused_at_load() {
    let mut s = grid(10.0, 600.0);
    s.events = vec![item(
        2.0,
        None,
        TimelineKind::ParamChange,
        json!({"path": "radio.rat", "value": "lte-v2x-mode4"}),
    )];
    let err = s.validate().expect_err("radio.rat is built once");
    assert!(err.to_string().contains("events[0].path"), "{err}");
    assert!(
        err.to_string().contains("cannot change during a run"),
        "{err}"
    );
    // A value that does not fit the parameter is refused too.
    s.events[0] = item(
        2.0,
        None,
        TimelineKind::ParamChange,
        json!({"path": "actors.vehicles.equipped_fraction", "value": 1.5}),
    );
    let err = s.validate().expect_err("a fraction above one");
    assert!(err.to_string().contains("events[0].value"), "{err}");
}

#[test]
fn a_demand_multiplier_needs_an_arrival_process_and_an_attack_wave_needs_its_population() {
    let mut s = grid(10.0, 600.0);
    s.actors.vehicles.demand.kind = "mobility/demand/none".to_string();
    s.actors.vehicles.demand.rate_veh_per_h = None;
    s.events = vec![item(
        1.0,
        None,
        TimelineKind::DemandMultiplier,
        json!({"value": 2.0}),
    )];
    let err = s.validate().expect_err("no arrival process");
    assert!(err.to_string().contains("events[0].type"), "{err}");

    let mut s = grid(10.0, 600.0);
    s.events = vec![item(
        1.0,
        None,
        TimelineKind::AttackWave,
        json!({"ids": [0]}),
    )];
    let err = s.validate().expect_err("no attacker population");
    assert!(err.to_string().contains("events[0].ids"), "{err}");
}

/// The weather front was wired before; this pins that it still records what it did, with
/// the rest of the timeline.
#[test]
fn every_item_writes_one_record_per_edge_it_has() {
    let mut s = grid(20.0, 600.0);
    s.events = vec![
        item(
            2.0,
            Some(8.0),
            TimelineKind::WeatherFront,
            json!({"value": "fog"}),
        ),
        item(
            3.0,
            None,
            TimelineKind::ParamChange,
            json!({"path": "weather.intensity", "value": 0.5}),
        ),
    ];
    // A weather front may not have an `until` (it has no end), so this is refused...
    assert!(s.validate().is_err());
    // ...and without it, two items write two records.
    s.events[0].until = None;
    let r = run(s);
    assert_eq!(r.events.len(), 2, "{:?}", r.events);
    assert_eq!(r.events[0]["kind"], "weather.front");
    assert_eq!(r.events[1]["kind"], "param.change");
    assert_eq!(r.events[1]["path"], "weather.intensity");
}
