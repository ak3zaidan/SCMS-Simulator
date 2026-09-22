//! Shared setup for the integration tests.

#![allow(dead_code)]

use v2xw_core::ids::NodeId;
use v2xw_proto::scms::params::ScmsParams;
use v2xw_proto::scms::run::ScmsRun;

/// The first device's node id. Backend roles occupy 1..=11.
pub const DEVICE_A: NodeId = NodeId::new(1_000);
/// A second device.
pub const DEVICE_B: NodeId = NodeId::new(1_001);
/// A third.
pub const DEVICE_C: NodeId = NodeId::new(1_002);

/// A deployment with `devices` devices and the short batching windows of
/// [`ScmsParams::quick`]. Every cited number is left alone.
pub fn deployment(devices: u32) -> ScmsRun {
    let mut run = ScmsRun::new(ScmsParams::default().quick()).expect("certificates encode");
    for k in 0..devices {
        run.add_device(NodeId::new(1_000 + k));
    }
    run
}

/// Enrols and provisions one device, then runs to quiescence.
pub fn provisioned(run: &mut ScmsRun, device: NodeId, start_i: u32, periods: u32, jmax: u32) {
    run.enrol(device);
    run.run().expect("enrolment runs");
    run.provision(device, start_i, periods, jmax);
    run.run().expect("provisioning runs");
}
