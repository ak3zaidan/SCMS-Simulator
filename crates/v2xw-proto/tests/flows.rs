//! Every flow emits the stages it declares, in the order it declares them.
//!
//! This is invariant I-P4 (05-protocols §7) as a test rather than a promise: the stage
//! lists in `scms::FLOWS` and `etsi::ts102941::FLOWS` are the contract, and a flow that
//! stops emitting one — or emits them out of order — fails here.

mod common;

use common::{DEVICE_A, DEVICE_B, deployment, provisioned};
use v2xw_core::ids::NodeId;
use v2xw_proto::scms::{self, FLOWS};
use v2xw_proto::stage::{FlowId, StageId};

fn stages_of(run: &scms::ScmsRun, flow: FlowId) -> Vec<Vec<StageId>> {
    run.kernel
        .stages
        .runs_of(flow)
        .into_iter()
        .map(|r| run.kernel.stages.stages(r))
        .collect()
}

fn declared(flow: FlowId) -> &'static [StageId] {
    FLOWS
        .iter()
        .find(|f| f.id == flow)
        .map(|f| f.stages)
        .expect("flow is declared")
}

#[test]
fn enrolment_emits_its_declared_stages_in_order() {
    let mut run = deployment(1);
    run.enrol(DEVICE_A);
    run.run().expect("runs");
    let seen = stages_of(&run, FlowId::Enrolment);
    assert_eq!(seen.len(), 1, "one run of the flow");
    assert_eq!(seen[0], declared(FlowId::Enrolment));
    let flow_run = run.kernel.stages.runs_of(FlowId::Enrolment)[0];
    assert!(run.kernel.stages.is_ordered(flow_run));
}

#[test]
fn provisioning_emits_its_declared_stages_in_order() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 3);
    let seen = stages_of(&run, FlowId::Provisioning);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], declared(FlowId::Provisioning));
    let flow_run = run.kernel.stages.runs_of(FlowId::Provisioning)[0];
    assert!(
        run.kernel.stages.is_ordered(flow_run),
        "stage timestamps must be non-decreasing: {:?}",
        run.kernel.stages.decomposition(flow_run)
    );
}

#[test]
fn topup_is_the_provisioning_machine_and_emits_the_same_stages() {
    let mut run = deployment(1);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    run.topup(DEVICE_A, 1, 2);
    run.run().expect("runs");
    let seen = stages_of(&run, FlowId::Topup);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], declared(FlowId::Topup));
    // And it really added credentials for the new period.
    let dev = &run.state.devices[&DEVICE_A];
    assert!(dev.credentials.contains_key(&(1, 0)));
}

#[test]
fn report_emits_its_declared_stages_in_order() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 1, 2);
    let lv = run.state.devices[&DEVICE_A].credentials[&(0, 0)].lv;
    run.submit_report(DEVICE_B, 0, lv);
    run.run().expect("runs");
    let seen = stages_of(&run, FlowId::Report);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], declared(FlowId::Report));
}

#[test]
fn resolution_and_crl_issuance_emit_their_declared_stages_in_order() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 2, 2);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("reports run");
    let (res, crl) = run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("investigation runs");

    assert_eq!(
        run.kernel.stages.stages(res),
        declared(FlowId::LinkageResolution)
    );
    assert!(run.kernel.stages.is_ordered(res));
    assert_eq!(run.kernel.stages.stages(crl), declared(FlowId::CrlIssuance));
    assert!(run.kernel.stages.is_ordered(crl));
}

#[test]
fn crl_distribution_emits_its_declared_stages_in_order() {
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 2, 2);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("runs");
    run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");
    let dist = run.distribute_crl(DEVICE_A);
    run.run().expect("runs");
    assert_eq!(
        run.kernel.stages.stages(dist),
        declared(FlowId::CrlDistribution)
    );
    assert!(run.kernel.stages.is_ordered(dist));
}

#[test]
fn the_revocation_decomposition_is_computable_end_to_end() {
    // What 05-protocols §8 exists for: detect → … → enforced, as differences between
    // stamped stages, across four flows and five organisations.
    let mut run = deployment(2);
    provisioned(&mut run, DEVICE_A, 0, 2, 2);
    let (lv0, lv1) = {
        let d = &run.state.devices[&DEVICE_A];
        (d.credentials[&(0, 0)].lv, d.credentials[&(0, 1)].lv)
    };
    let report = run.submit_report(DEVICE_B, 0, lv0);
    run.submit_report(DEVICE_B, 0, lv1);
    run.run().expect("runs");
    let (res, crl) = run.investigate(0, 1, 1, 2).expect("two reports");
    run.run().expect("runs");
    let dist = run.distribute_crl(DEVICE_A);
    run.run().expect("runs");

    let detect = run
        .kernel
        .stages
        .at(report, StageId::Detect)
        .expect("detect");
    let received = run
        .kernel
        .stages
        .at(report, StageId::ReportReceived)
        .expect("report_received");
    let decision = run
        .kernel
        .stages
        .at(res, StageId::Decision)
        .expect("decision");
    let resolved = run
        .kernel
        .stages
        .at(res, StageId::Resolved)
        .expect("resolved");
    let issued = run.kernel.stages.at(crl, StageId::Issued).expect("issued");
    let published = run
        .kernel
        .stages
        .at(crl, StageId::Published)
        .expect("published");
    let enforced = run
        .kernel
        .stages
        .at_node(dist, StageId::Enforced, DEVICE_A)
        .expect("enforced at the device");

    for (name, a, b) in [
        ("detect → report_received", detect, received),
        ("decision → resolved", decision, resolved),
        ("resolved → issued", resolved, issued),
        ("issued → published", issued, published),
        ("published → enforced", published, enforced),
    ] {
        assert!(a <= b, "{name}: {a} must not be after {b}");
    }
    assert!(
        enforced > detect,
        "the whole path must take a positive amount of simulated time"
    );
}

#[test]
fn etsi_flows_emit_their_declared_stages_in_order() {
    use v2xw_proto::etsi::ts102941::{EtsiParams, EtsiRun, FLOWS as ETSI_FLOWS};
    let station = NodeId::new(2_000);
    let mut run = EtsiRun::new(EtsiParams::default()).expect("certificates encode");
    run.add_station(station);
    let enrol = run.enrol(station);
    run.run().expect("runs");
    let auth = run.authorize(station);
    run.run().expect("runs");

    let expect = |id: v2xw_proto::stage::FlowId| {
        ETSI_FLOWS
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.stages)
            .expect("declared")
    };
    assert_eq!(
        run.kernel.stages.stages(enrol),
        expect(FlowId::EtsiEnrolment)
    );
    assert_eq!(
        run.kernel.stages.stages(auth),
        expect(FlowId::EtsiAuthorization)
    );
    assert!(run.kernel.stages.is_ordered(enrol));
    assert!(run.kernel.stages.is_ordered(auth));
    assert_eq!(run.tickets.get(&station), Some(&1));
}
