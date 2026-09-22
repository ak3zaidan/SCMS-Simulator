//! Determinism: the same seed gives the same run, byte for byte and stage for stage.
//!
//! And the negative half, which is what makes it a test rather than a tautology: a
//! different seed must give different cryptographic material. A run that were deterministic
//! because nothing random happened would pass the first check and fail the second.

mod common;

use common::{DEVICE_A, DEVICE_B};
use v2xw_core::ids::NodeId;
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::scms::run::ScmsRun;

fn scenario(seed: u64) -> ScmsRun {
    let mut params = ScmsParams::default().quick();
    params.master_seed = seed;
    let mut run = ScmsRun::new(params).expect("encodes");
    for k in 0..2 {
        run.add_device(NodeId::new(1_000 + k));
    }
    run.enrol(DEVICE_A);
    run.run().expect("runs");
    run.provision(DEVICE_A, 0, 3, 2);
    run.run().expect("runs");
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("runs");
    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");
    run.distribute_crl(DEVICE_A);
    run.run().expect("runs");
    run
}

#[test]
fn the_same_seed_reproduces_every_stage_and_every_byte() {
    let a = scenario(0xC0FFEE);
    let b = scenario(0xC0FFEE);
    assert_eq!(a.kernel.stages.stamps(), b.kernel.stages.stamps());
    assert_eq!(a.kernel.steps, b.kernel.steps);
    assert_eq!(a.kernel.ops, b.kernel.ops);
    assert_eq!(
        a.state.crl_store.entries.len(),
        b.state.crl_store.entries.len()
    );
    assert_eq!(a.crl_entry(), b.crl_entry());
}

#[test]
fn a_different_seed_changes_the_cryptographic_material_but_not_the_shape() {
    let a = scenario(1);
    let b = scenario(2);
    // The shape is identical: the same flows ran, the same messages crossed the same
    // links at the same instants.
    assert_eq!(a.kernel.steps, b.kernel.steps);
    assert_eq!(a.kernel.stages.stamps(), b.kernel.stages.stamps());
    // The material is not.
    let lv_a = a.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
    let lv_b = b.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
    assert_ne!(
        lv_a.as_bytes(),
        lv_b.as_bytes(),
        "a different master seed must give different linkage values"
    );
    assert_ne!(a.crl_entry(), b.crl_entry());
}

#[test]
fn every_flow_run_is_a_timeline() {
    // Per run, not globally: a backend entity has several servers, so two flows it is
    // serving at once finish in whichever order their service times give — which is the
    // truth about a four-server Misbehaviour Authority, not a defect. What must hold is
    // that no single flow's stages ever go backwards, because that is what a latency
    // decomposition is computed from.
    let run = scenario(7);
    let stamps = run.kernel.stages.stamps();
    assert!(
        stamps.len() > 20,
        "the scenario exercised only {} stages",
        stamps.len()
    );
    let mut runs: Vec<_> = stamps.iter().map(|s| s.run).collect();
    runs.sort_unstable();
    runs.dedup();
    assert!(runs.len() >= 6, "several flows ran");
    for r in runs {
        assert!(
            run.kernel.stages.is_ordered(r),
            "{r} went backwards: {:?}",
            run.kernel.stages.decomposition(r)
        );
    }
}

#[test]
fn queueing_delay_appears_when_entities_are_loaded() {
    // Four servers at the PCA; provisioning twenty devices at once must make some request
    // wait, or the queue is not a queue.
    let mut params = ScmsParams::default().quick();
    params.master_seed = 42;
    let mut run = ScmsRun::new(params).expect("encodes");
    for k in 0..20 {
        run.add_device(NodeId::new(1_000 + k));
    }
    for k in 0..20 {
        let d = NodeId::new(1_000 + k);
        run.enrol(d);
    }
    run.run().expect("runs");
    for k in 0..20 {
        let d = NodeId::new(1_000 + k);
        run.provision(d, 0, 1, 20);
    }
    run.run().expect("runs");

    let pca = run.kernel.queue(run.state.nodes.pca).expect("hosted");
    assert_eq!(pca.served(), 20);
    assert!(
        pca.waiting().as_nanos() > 0,
        "twenty simultaneous batches of twenty certificates must queue at four servers"
    );
    assert!(pca.busy().as_nanos() > 0);
    assert_eq!(run.state.pca.issued_count, 400);
}
