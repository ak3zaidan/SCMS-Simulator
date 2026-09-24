//! An `attack.wave` on the timeline is when its attacker populations act.
//!
//! The wave is dispatched through the threat crate's own gate: it becomes the population's
//! `AttackSchedule` (`crates/v2xw-engine/src/timeline.rs`, `attack_windows`), which the
//! attacker model enforces itself. The check is on what the attacker did, counted by the
//! Phase 2 report (`falsified_claims`): on its own schedule the attacker falsifies claims
//! before the wave's start — the control that shows the check can fail — while under the
//! wave it falsifies nothing before the wave and does falsify inside it.

use std::path::Path;

use serde_json::json;
use v2xw_engine::scenario::schema::{TimelineItem, TimelineKind};
use v2xw_engine::{Engine, NullRecorder, Scenario};

fn phase2(duration_s: f64) -> Scenario {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut s = Scenario::load(root.join("scenarios").join("phase2-manhattan.yaml"))
        .expect("the shipped scenario loads");
    if let v2xw_world::WorldSourceSpec::OsmXml { path, .. } = &mut s.world.source
        && Path::new(path).is_relative()
    {
        *path = root.join(&*path).to_string_lossy().into_owned();
    }
    s.time.duration_s = duration_s;
    for a in &mut s.threats.attackers {
        if let Some(w) = a.schedule.as_mut() {
            w.to_s = w.to_s.min(duration_s);
        }
    }
    s
}

fn falsified(s: Scenario) -> u64 {
    let mut engine = Engine::build(s, "").expect("builds");
    let report = engine.run(&mut NullRecorder::new()).expect("runs");
    assert_eq!(report.phase2.attackers, 1, "the attacker was armed");
    report.phase2.falsified_claims
}

fn wave(mut s: Scenario, from: f64, to: Option<f64>) -> Scenario {
    s.events = vec![TimelineItem {
        t: from,
        until: to,
        kind: TimelineKind::AttackWave,
        params: [("ids".to_string(), json!([0]))].into_iter().collect(),
    }];
    s
}

#[test]
fn an_attack_wave_is_when_its_population_acts() {
    const FROM: f64 = 30.0;
    const TO: f64 = 50.0;
    // The control: on its own schedule (from 5 s) the attacker is busy before 30 s.
    let own = falsified(phase2(FROM));
    assert!(
        own > 0,
        "the attacker falsifies on its own schedule before {FROM} s"
    );
    // Under the wave, nothing happens before the wave starts...
    let before = falsified(wave(phase2(FROM), FROM, None));
    assert_eq!(before, 0, "{before} claims falsified before the wave began");
    // ...and the attacker acts once it has.
    let during = falsified(wave(phase2(TO), FROM, Some(TO)));
    assert!(during > 0, "the attacker acted during the wave");
    println!(
        "attack wave [{FROM}, {TO}) s: {own} claims falsified by {FROM} s on the attacker's own \
         schedule; 0 with the wave; {during} by {TO} s with it"
    );
}
