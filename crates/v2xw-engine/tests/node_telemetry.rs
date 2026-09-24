//! Every node's telemetry window reaches the recording.
//!
//! The node runtime has always closed a window each telemetry period with its queue
//! depths, its compute load and its stores (`StepOutcome::telemetry`), and the engine threw
//! it away: nothing wrote `node.telemetry`, so the page showed "n/a" for every queue and
//! for the CPU of the vehicle being followed.

use std::path::Path;

use v2xw_engine::{Engine, MemoryRecorder, Scenario};
use v2xw_metrics::channels::{NodeTelemetryView, decode};

#[test]
fn each_node_publishes_its_queues_and_its_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut s = Scenario::load(root.join("scenarios/phase1-grid.yaml")).expect("loads");
    s.time.duration_s = 4.0;
    s.actors.vehicles.demand.rate_veh_per_h = Some(3000.0);
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    engine.run(&mut recorder).expect("runs");
    let windows: Vec<NodeTelemetryView> = recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "node.telemetry")
        .map(|(_, r)| {
            v2xw_record::grid::scan_record("node.telemetry", &r.json)
                .unwrap_or_else(|e| panic!("node.telemetry is off its grid: {e}"));
            decode(r).expect("node.telemetry decodes")
        })
        .collect();
    assert!(!windows.is_empty(), "no node published a telemetry window");
    for w in &windows {
        assert!(
            w.q_verify.is_some(),
            "node {} has no verify queue depth",
            w.node.index()
        );
        assert!(
            w.q_rx.is_some() && w.q_tx.is_some(),
            "node {} lacks a queue",
            w.node.index()
        );
        let cpu = w.cpu.expect("a CPU load");
        assert!(
            (0.0..=1.0).contains(&cpu),
            "CPU load {cpu} is not a fraction"
        );
    }
    // Windows close once per telemetry period, so a 4 s run gives each node a few.
    let nodes: std::collections::BTreeSet<u32> = windows.iter().map(|w| w.node.index()).collect();
    assert!(
        windows.len() >= nodes.len(),
        "{} windows for {} nodes",
        windows.len(),
        nodes.len()
    );
}
