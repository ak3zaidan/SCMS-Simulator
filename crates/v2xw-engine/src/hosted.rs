//! The devices the kernel's node phase hosts: a vehicle's OBU, or a vulnerable road
//! user's handset.
//!
//! `actors.vru.device_fraction` used to be refused because the node phase could only hold
//! an [`ObuRuntime`], and a pedestrian carrying one would have put vehicle BSMs on the air.
//! [`HostedNode`] lets the node phase hold either. It answers every question the engine
//! asks a node — its clock, its stores, its belief, its state — with the same method names
//! the OBU has, so the engine's radio, security and metrics paths treat a pedestrian's
//! PSM or VAM exactly as they treat a vehicle's BSM or CAM: the same `node.tx` and
//! `node.rx` records, the same MAC, PHY and congestion control, the same metrics.
//!
//! What differs is inside the device, and is `v2xw_node::vru`'s: the SAE J2945/9 PSM
//! cadence or the ETSI TS 103 300-3 VAM triggers, the EN 302 571 duty-cycle gate and the
//! energy budget.
//!
//! # Which messages a device sends
//!
//! The scenario's message sets choose the stack, as they do for a vehicle:
//!
//! | `messages.sets` contains | The device sends |
//! |---|---|
//! | `psm`, or `bsm` (the SAE stack) | PSM (SAE J2735, J2945/9 cadence) |
//! | `vam`, or `cam` (the ETSI stack) | VAM (ETSI TS 103 300-3) |
//!
//! A scenario carrying both stacks gets both, as a dual-stack handset would.
//!
//! # What a suppressed message leaves behind
//!
//! The device's own `VruSuppressionRecord` rides `node.tx` in `v2xw-node`, but `node.tx`
//! in a recording is the engine's `NodeTx` shape, which the dataset assembler and the
//! metric providers decode; a second shape on the channel would fail them. So the host
//! does not write those records: a suppression is counted in the run report
//! ([`crate::RunReport::vru_suppressed`]) and as a transmit drop in the device's own
//! `node.telemetry` window.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geom::Dims;
use v2xw_core::ids::NodeId;
use v2xw_core::nodeview::NodeView;
use v2xw_core::time::SimTime;
use v2xw_mobility::VehicleClass;
use v2xw_msg::generator::DccState;
use v2xw_node::stores::{CredState, CredentialHandle, NeighborTable};
use v2xw_node::vru::{VruConfig, VruDeviceKind, VruDeviceRuntime, VruServices};
use v2xw_node::{
    HardwareProfile, NodeCtx, NodeSecurity, NodeState, ObuRuntime, RxFrame, RxStamp, StepOutcome,
    Stores, VerifiedMessage,
};

use crate::scenario::Scenario;
use crate::wiring::NodeEnv;

/// One device the node phase steps.
pub enum HostedNode {
    /// A vehicle's on-board unit.
    Obu(Box<ObuRuntime>),
    /// A pedestrian's or a cyclist's V2X device.
    Vru(Box<VruDeviceRuntime>),
}

impl core::fmt::Debug for HostedNode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostedNode::Obu(_) => f.write_str("HostedNode::Obu"),
            HostedNode::Vru(v) => write!(f, "HostedNode::Vru({v:?})"),
        }
    }
}

/// Calls the same method on whichever device this is.
macro_rules! both {
    ($self:expr, $n:ident => $e:expr) => {
        match $self {
            HostedNode::Obu($n) => $e,
            HostedNode::Vru($n) => $e,
        }
    };
}

impl From<ObuRuntime> for HostedNode {
    fn from(o: ObuRuntime) -> Self {
        HostedNode::Obu(Box::new(o))
    }
}

impl From<VruDeviceRuntime> for HostedNode {
    fn from(v: VruDeviceRuntime) -> Self {
        HostedNode::Vru(Box::new(v))
    }
}

impl HostedNode {
    /// The OBU, if this is one.
    pub fn as_obu(&self) -> Option<&ObuRuntime> {
        match self {
            HostedNode::Obu(o) => Some(o),
            HostedNode::Vru(_) => None,
        }
    }

    /// The OBU, mutably, if this is one.
    pub fn as_obu_mut(&mut self) -> Option<&mut ObuRuntime> {
        match self {
            HostedNode::Obu(o) => Some(o),
            HostedNode::Vru(_) => None,
        }
    }

    /// The VRU device, if this is one.
    pub fn as_vru(&self) -> Option<&VruDeviceRuntime> {
        match self {
            HostedNode::Vru(v) => Some(v),
            HostedNode::Obu(_) => None,
        }
    }

    /// True for a pedestrian's or a cyclist's device.
    pub fn is_vru(&self) -> bool {
        matches!(self, HostedNode::Vru(_))
    }

    /// The device's clock model.
    pub fn clock(&self) -> &v2xw_node::ClockModel {
        both!(self, n => n.clock())
    }

    /// The device's stores.
    pub fn stores(&self) -> &Stores {
        both!(self, n => n.stores())
    }

    /// The device's stores, mutably.
    pub fn stores_mut(&mut self) -> &mut Stores {
        both!(self, n => n.stores_mut())
    }

    /// The security stack.
    pub fn security(&self) -> &NodeSecurity {
        both!(self, n => n.security())
    }

    /// The security stack, mutably.
    pub fn security_mut(&mut self) -> &mut NodeSecurity {
        both!(self, n => n.security_mut())
    }

    /// The hardware profile.
    pub fn profile(&self) -> &HardwareProfile {
        both!(self, n => n.profile())
    }

    /// The device's state.
    pub fn state(&self) -> NodeState {
        both!(self, n => n.state())
    }

    /// Moves the device to another state.
    pub fn set_state(&mut self, state: NodeState) {
        both!(self, n => n.set_state(state));
    }

    /// Hands the device this step's position belief.
    pub fn set_belief(&mut self, belief: PositionEstimate) {
        both!(self, n => n.set_belief(belief));
    }

    /// Hands in the ground-truth position error, for telemetry only.
    pub fn observe_truth(&mut self, pos_error_m: f32) {
        both!(self, n => n.observe_truth(pos_error_m));
    }

    /// Sets the congestion-control state the generator honours.
    pub fn set_dcc(&mut self, dcc: DccState, state_code: u16) {
        both!(self, n => n.set_dcc(dcc, state_code));
    }

    /// SPDUs this device could not parse (zero for a VRU device, which does not count them).
    pub fn spdu_parse_failures(&self) -> u64 {
        self.as_obu().map_or(0, ObuRuntime::spdu_parse_failures)
    }

    /// SPDUs whose signature failed (zero for a VRU device, which does not count them).
    pub fn spdu_signature_failures(&self) -> u64 {
        self.as_obu().map_or(0, ObuRuntime::spdu_signature_failures)
    }

    /// One node-phase step, with each frame's reception stamp. A VRU device's outcome is
    /// returned in the OBU's shape; how many messages it suppressed comes back beside it.
    pub fn step_timed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<(RxFrame, RxStamp)>,
        distance_travelled_m: f64,
    ) -> (StepOutcome, u64) {
        match self {
            HostedNode::Obu(o) => (o.step_timed(ctx, inbox, distance_travelled_m), 0),
            HostedNode::Vru(v) => {
                let out = v.step_timed(ctx, inbox, distance_travelled_m);
                let suppressed = out.suppressed.len() as u64;
                (
                    StepOutcome {
                        transmissions: out.transmissions,
                        delivered: out.delivered,
                        telemetry: out.telemetry,
                        rx_reports: out.rx_reports,
                    },
                    suppressed,
                )
            }
        }
    }
}

impl HostedNode {
    /// Wakes the node between its periodic steps to hand over finished signature checks
    /// ([`ObuRuntime::wake_timed`]). A VRU device verifies within its own step and has no
    /// deferred checks, so a wake does nothing for it; the engine leaves its inbox for its
    /// next periodic step.
    pub fn wake_timed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<(RxFrame, RxStamp)>,
    ) -> StepOutcome {
        match self {
            HostedNode::Obu(o) => o.wake_timed(ctx, inbox),
            HostedNode::Vru(_) => {
                debug_assert!(inbox.is_empty(), "a VRU device's inbox waits for its step");
                StepOutcome::default()
            }
        }
    }

    /// When the engine must wake this node for its next finished signature check
    /// ([`ObuRuntime::next_completion_after`]); `None` for a VRU device, which has none.
    pub fn next_completion_after(&self, now: SimTime) -> Option<SimTime> {
        self.as_obu().and_then(|o| o.next_completion_after(now))
    }
}

impl NodeView for HostedNode {
    type Neighbors = NeighborTable;
    type Credential = CredentialHandle;
    type Message = VerifiedMessage;

    fn node(&self) -> NodeId {
        both!(self, n => n.node())
    }

    fn believed_time(&self) -> SimTime {
        both!(self, n => n.believed_time())
    }

    fn position(&self) -> &PositionEstimate {
        both!(self, n => n.position())
    }

    fn neighbors(&self) -> &NeighborTable {
        both!(self, n => n.neighbors())
    }

    fn credentials(&self) -> &[CredentialHandle] {
        both!(self, n => n.credentials())
    }

    fn received(&self) -> &[VerifiedMessage] {
        both!(self, n => n.received())
    }
}

/// The hardware profile every VRU device is built on (`vru-device/handset-generic`).
pub const VRU_DEVICE_PROFILE: &str = v2xw_node::profiles::GENERIC_VRU_DEVICE;

/// Which awareness services a VRU device runs under this scenario's message sets.
pub fn vru_services(scenario: &Scenario) -> VruServices {
    let has = |s: &str| scenario.messages.sets.iter().any(|x| x == s);
    let psm = has("psm") || has("bsm");
    let vam = has("vam") || has("cam");
    match (psm, vam) {
        (true, true) => VruServices::BOTH,
        (true, false) => VruServices::SAE,
        (false, true) => VruServices::ETSI,
        // A scenario with neither stack's safety message has no stack to pick: the SAE one,
        // the default `messages.sets: [bsm]` implies.
        (false, false) => VruServices::SAE,
    }
}

/// Builds a pedestrian's or a cyclist's device: the shipped `vru-device/handset-generic`
/// profile, a handset (it receives as well as transmits), the scenario's security profile
/// and pseudonym-rotation rule, and the same bootstrap batch of pseudonyms a vehicle gets.
pub fn build_vru_device(
    scenario: &Scenario,
    env: NodeEnv,
    node: NodeId,
    at: SimTime,
    class: VehicleClass,
    dims: Dims,
) -> VruDeviceRuntime {
    let profile = v2xw_node::profiles::get(v2xw_node::profiles::GENERIC_VRU_DEVICE)
        .expect("the generic VRU-device profile ships with v2xw-node")
        .clone();
    let config = VruConfig {
        kind: VruDeviceKind::Handset,
        services: vru_services(scenario),
        crypto_mode: crate::wiring::crypto_mode(scenario),
        wall: env.wall,
        origin: env.origin,
        dims,
        psm_user_type: if class == VehicleClass::Bicycle {
            v2xw_msg::j2735::psm::PersonalDeviceUserType::Pedalcyclist
        } else {
            v2xw_msg::j2735::psm::PersonalDeviceUserType::Pedestrian
        },
        ..VruConfig::for_kind(VruDeviceKind::Handset)
    };
    let mut device = VruDeviceRuntime::new(node, profile, config, at);
    let policy = crate::wiring::signer_id_policy(scenario);
    *device.security_mut() = NodeSecurity::new(
        env.wall,
        crate::wiring::crypto_mode(scenario),
        v2xw_node::secure::PSID_SAFETY,
    )
    .with_profile(crate::wiring::envelope_profile(scenario), env.wall)
    .with_signer_id_policies(policy, policy);
    install_credentials(
        device.stores_mut(),
        scenario,
        node,
        (0..crate::wiring::BOOTSTRAP_PSEUDONYMS).map(|j| (0, j, at, SimTime::MAX)),
    );
    device
}

/// Replaces a device's pseudonym store with `creds` (`(i, j, valid_from, valid_until)`),
/// under the scenario's rotation rule — the bootstrap batch, or what the SCMS provisioned.
/// The digest, key handle and certificate size are the ones `wiring::bootstrap_credentials`
/// gives a vehicle.
pub fn install_credentials(
    stores: &mut Stores,
    scenario: &Scenario,
    node: NodeId,
    creds: impl IntoIterator<Item = (u32, u32, SimTime, SimTime)>,
) {
    let policy = crate::wiring::rotation_policy(scenario);
    *stores = Stores {
        certs: v2xw_node::CertStore::new().with_policy(policy),
        ..core::mem::take(stores)
    };
    for (i, j, from, until) in creds {
        stores.certs.insert(CredentialHandle {
            digest: v2xw_node::stores::pseudo_signer(node, j),
            cert_coer: vec![0u8; 117],
            key: v2xw_sec::KeyId(u64::from(node.index()) << 8 | u64::from(j)),
            i_period: i,
            j_index: j,
            valid_from: from,
            valid_until: until,
            state: CredState::Active,
        });
    }
}
