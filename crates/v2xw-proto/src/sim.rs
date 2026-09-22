//! Driving the credential backend from a running simulation.
//!
//! [`crate::scms::run::ScmsRun`] is a deployment on its own kernel, which is what the
//! flow, privacy and revocation tests need. An engine has its own clock and its own event
//! loop, and it needs three things this module adds and nothing else:
//!
//! 1. **A horizon.** [`CredentialService::advance_to`] does the backend work that was due
//!    by an instant and stops. A round trip left in flight stays in flight, which is the
//!    difference between provisioning that costs simulated time and provisioning that
//!    completes inside one engine step.
//! 2. **Records to emit.** [`CredentialService::drain`] hands back the stage stamps, the
//!    per-hop message records and the pseudonym-change events made since the last call,
//!    each already a `v2xw_core::ctx::Record` on its documented channel
//!    (03-interfaces.md §14). A cursor, not a drain of the log: the log is what a
//!    decomposition is computed from and stays whole.
//! 3. **Credentials in a shape a node can hold.** [`Pseudonym`] is exactly the fields
//!    `v2xw_node::stores::CredentialHandle` needs — digest-sized certificate bytes, the
//!    `(i, j)` position, the validity window — so the engine's node store is filled from
//!    the flow that really issued them instead of from a stand-in.
//!
//! # What the engine does, in order
//!
//! ```text
//! at load:        let mut cred = CredentialService::new(params, scenario.time.t0)?;
//! per spawn:      cred.bootstrap(node, now, strategy);        // enrol, then provision
//! per event loop: cred.advance_to(now)?;                      // backend work due by now
//!                 for r in cred.drain() { ctx.emit(r) }       // proto.*, sec.cert
//! per node step:  cred.travelled_cm(node, cm);
//!                 if let Some(e) = cred.rotate(node, now) { ctx.emit(e) }
//!                 for p in cred.installed(node) { store.insert(p.into_handle(..)) }
//! ```
//!
//! **Determinism.** Nothing here reads a clock: every instant is one the caller supplied
//! or one the kernel computed from a service time and a link delay. Every collection that
//! reaches an output is a [`BTreeMap`] or a `Vec` in insertion order.

use std::collections::BTreeMap;

use v2xw_core::ids::NodeId;
use v2xw_core::time::{Duration, SimTime};

use crate::error::Result;
use crate::pseudonym::{CertEvent, PseudonymStrategy};
use crate::scms::params::ScmsParams;
use crate::scms::run::ScmsRun;
use crate::stage::{FlowId, FlowRun, StageId, StageLog, StageStamp, WireStep};

/// One pseudonym certificate a device holds, in the shape a node's store wants.
///
/// The certificate *bytes* are not here. The engine's envelope charges for a certificate
/// by its encoded length (04-models.md §9.1) and a modelled-crypto run never parses one,
/// so this carries [`Pseudonym::cert_bytes`] — the length the real COER encoder produced
/// for this deployment's certificate profile — and the caller allocates. A `real`-crypto
/// run needs the bytes themselves, which is a build-decision-D11 seam and not a field to
/// invent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Pseudonym {
    /// The i-period this certificate belongs to.
    pub i_period: u32,
    /// Its index within the period — the `j` of the butterfly expansion.
    pub j_index: u32,
    /// The start of its validity window.
    pub valid_from: SimTime,
    /// The end of its validity window.
    pub valid_until: SimTime,
    /// The encoded length of the certificate, from the real encoder.
    pub cert_bytes: u32,
    /// Its linkage value: the identifier a CRL entry matches and the only identity a
    /// receiver of a message signed with it can see.
    pub linkage_value: [u8; 9],
    /// Whether the device could sign with it at the instant it was asked.
    pub usable: bool,
    /// Whether the CRL this device has processed revokes it.
    pub revoked: bool,
}

/// The two flow runs a device's start-up creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bootstrap {
    /// The device.
    pub device: NodeId,
    /// The enrolment run.
    pub enrolment: FlowRun,
    /// The pseudonym-provisioning run.
    pub provisioning: FlowRun,
}

/// Everything the service produced since it was last asked.
///
/// Three `Vec`s rather than one of a sum type, because each lands on a different channel
/// and the engine's recorder wants them typed.
#[derive(Debug, Clone, Default)]
pub struct Drained {
    /// `proto.revocation` — every stage stamped, provisioning and revocation alike.
    pub stages: Vec<StageStamp>,
    /// `proto.msg` — every hop, with its bytes and its transport.
    pub steps: Vec<WireStep>,
    /// `sec.cert` — every pseudonym change.
    pub certs: Vec<CertEvent>,
}

impl Drained {
    /// How many records this batch holds in total.
    pub fn len(&self) -> usize {
        self.stages.len() + self.steps.len() + self.certs.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The credential backend, driven by a foreign event loop.
pub struct CredentialService {
    run: ScmsRun,
    stage_cursor: usize,
    step_cursor: usize,
    certs: Vec<CertEvent>,
    bootstraps: BTreeMap<NodeId, Bootstrap>,
    periods: u32,
    jmax: u32,
}

impl CredentialService {
    /// A service for `params` whose clock starts at `t0`.
    ///
    /// # Errors
    /// [`crate::ProtoError::Size`] if the certificate profile does not encode.
    pub fn new(params: ScmsParams, t0: SimTime) -> Result<CredentialService> {
        let periods = 1;
        let jmax = params.certs_per_period;
        Ok(CredentialService {
            run: ScmsRun::new_at(params, t0)?,
            stage_cursor: 0,
            step_cursor: 0,
            certs: Vec::new(),
            bootstraps: BTreeMap::new(),
            periods,
            jmax,
        })
    }

    /// How many i-periods and how many certificates per period a bootstrap asks for.
    ///
    /// The shipped default is one period of `certs_per_period` — 20 certificates, one
    /// week, [CAMP-EE Table 2.1.2.6.2] — rather than the 3,120-certificate initial batch
    /// of [PRIMER p.7], because a 60-second scenario that provisioned three years of
    /// certificates would spend its whole run inside one PCA batch and measure the batch
    /// rather than the vehicle. A scenario that wants the real initial batch says so here.
    #[must_use]
    pub const fn with_batch(mut self, periods: u32, jmax: u32) -> CredentialService {
        self.periods = periods;
        self.jmax = jmax;
        self
    }

    /// A service over a deployment that already exists.
    ///
    /// For a caller that built the deployment itself — a test walking the flows directly,
    /// or a scenario with a topology other than the reference one — and now wants the
    /// engine seam over it. The record cursors start at zero, so the first
    /// [`CredentialService::drain`] returns everything the deployment has produced so far.
    pub const fn from_run(run: ScmsRun) -> CredentialService {
        CredentialService {
            run,
            stage_cursor: 0,
            step_cursor: 0,
            certs: Vec::new(),
            bootstraps: BTreeMap::new(),
            periods: 1,
            jmax: 20,
        }
    }

    /// Every stage every flow of this deployment stamped.
    pub const fn stages(&self) -> &StageLog {
        &self.run.kernel.stages
    }

    /// The deployment's parameters.
    pub const fn params(&self) -> &ScmsParams {
        &self.run.state.params
    }

    /// The deployment, for a caller that needs to inspect an entity's state.
    pub const fn deployment(&self) -> &ScmsRun {
        &self.run
    }

    /// The deployment, mutably.
    pub const fn deployment_mut(&mut self) -> &mut ScmsRun {
        &mut self.run
    }

    /// The backend's current instant.
    pub const fn now(&self) -> SimTime {
        self.run.kernel.now()
    }

    /// Starts a device up at `at`: enrolment, then pseudonym provisioning.
    ///
    /// Both flows are injected at `at`, and the provisioning request is signed with the
    /// enrolment certificate, so the second flow's first message is dispatched after the
    /// first flow has delivered one — the ordering the kernel's `(time, sequence)` total
    /// order gives for free, since enrolment was injected first.
    pub fn bootstrap(
        &mut self,
        device: NodeId,
        at: SimTime,
        strategy: PseudonymStrategy,
    ) -> Bootstrap {
        self.run.add_device(device);
        self.run.set_strategy(device, strategy);
        let enrolment = self.run.enrol_at(device, at);
        let start_i = self.period_of(at);
        let provisioning = self
            .run
            .provision_at(device, at, start_i, self.periods, self.jmax);
        let b = Bootstrap {
            device,
            enrolment,
            provisioning,
        };
        self.bootstraps.insert(device, b);
        b
    }

    /// The bootstrap a device was started with.
    pub fn bootstrap_of(&self, device: NodeId) -> Option<Bootstrap> {
        self.bootstraps.get(&device).copied()
    }

    /// The i-period the instant `t` falls in.
    pub const fn period_of(&self, t: SimTime) -> u32 {
        let p = &self.run.state.params;
        if t <= p.epoch || p.i_period.as_nanos() == 0 {
            return 0;
        }
        let elapsed = Duration::between(p.epoch, t).as_nanos();
        let i = elapsed / p.i_period.as_nanos();
        if i > u32::MAX as u64 {
            u32::MAX
        } else {
            i as u32
        }
    }

    /// Does the backend work due at or before `t`.
    ///
    /// # Errors
    /// Whatever the kernel returns: an unhosted node, a missing link, or a flow driven out
    /// of order. All three are modelling defects the kernel refuses rather than papers over.
    pub fn advance_to(&mut self, t: SimTime) -> Result<()> {
        self.run.run_until(t)
    }

    /// When the backend next has work, if it has any.
    ///
    /// The engine can schedule its own wakeup from this instead of polling every step.
    pub fn next_due(&self) -> Option<SimTime> {
        self.run.kernel.next_due()
    }

    /// Everything to record since the last call.
    pub fn drain(&mut self) -> Drained {
        let (stages, sc) = self.run.kernel.stages_since(self.stage_cursor);
        let stages = stages.to_vec();
        self.stage_cursor = sc;
        let (steps, tc) = self.run.kernel.steps_since(self.step_cursor);
        let steps = steps.to_vec();
        self.step_cursor = tc;
        Drained {
            stages,
            steps,
            certs: core::mem::take(&mut self.certs),
        }
    }

    /// The pseudonyms `device` holds, judged usable as at `now`.
    pub fn installed(&self, device: NodeId, now: SimTime) -> Vec<Pseudonym> {
        let params = &self.run.state.params;
        let cert_bytes = self.run.state.sizes.certs.pseudonym.bytes();
        let Some(dev) = self.run.state.devices.get(&device) else {
            return Vec::new();
        };
        dev.credentials
            .iter()
            .map(|(&(i, j), c)| {
                let (valid_from, valid_until) = params.validity(i);
                let revoked = dev.is_revoked(i, j);
                Pseudonym {
                    i_period: i,
                    j_index: j,
                    valid_from,
                    valid_until,
                    cert_bytes,
                    linkage_value: *c.lv.as_bytes(),
                    usable: !revoked && now >= valid_from && now < valid_until,
                    revoked,
                }
            })
            .collect()
    }

    /// The pseudonym `device` would sign with now, if it has one.
    pub fn active(&self, device: NodeId, now: SimTime) -> Option<Pseudonym> {
        let (i, j) = self.run.state.devices.get(&device)?.store.active()?;
        self.installed(device, now)
            .into_iter()
            .find(|p| (p.i_period, p.j_index) == (i, j))
    }

    /// Adds travel since `device`'s last pseudonym change.
    pub fn travelled_cm(&mut self, device: NodeId, cm: u64) {
        self.run.travelled_cm(device, cm);
    }

    /// Tells `device` it has left a mix zone.
    pub fn left_mix_zone(&mut self, device: NodeId) {
        self.run.left_mix_zone(device);
    }

    /// Rotates `device`'s pseudonym if its strategy says one is due at `now`.
    ///
    /// The event is both returned and queued for the next [`CredentialService::drain`], so
    /// a caller that only records through the drain does not have to handle it twice.
    pub fn rotate(&mut self, device: NodeId, now: SimTime) -> Option<CertEvent> {
        let e = self.run.rotate(device, now)?;
        self.certs.push(e);
        Some(e)
    }

    /// Has `device` fetch and enforce the CRL over the cellular uplink.
    pub fn fetch_crl(&mut self, device: NodeId) -> FlowRun {
        self.run.distribute_crl(device)
    }

    /// Puts `device` in range of the roadside broadcast path.
    pub fn attach_rsu(&mut self, device: NodeId) {
        self.run.attach_rsu(device);
    }

    /// Has the roadside path broadcast the CRL to `device`.
    pub fn broadcast_crl(&mut self, device: NodeId) -> FlowRun {
        self.run.broadcast_crl_to(device)
    }

    /// One flow run's stage decomposition, in stamping order.
    pub fn decomposition(&self, run: FlowRun) -> Vec<(StageId, SimTime)> {
        self.run.kernel.stages.decomposition(run)
    }

    /// The provisioning cost of one run: every stage, and the bytes each side paid.
    pub fn provisioning_cost(&self, run: FlowRun) -> Option<ProvisioningCost> {
        let log = &self.run.kernel.stages;
        let at = |s: StageId| log.at(run, s);
        let requested = at(StageId::Requested)?;
        let installed = at(StageId::Installed)?;
        let mut bytes = BTreeMap::new();
        for step in &self.run.kernel.steps {
            if step.run == run {
                *bytes.entry(step.transport).or_insert(0u64) += u64::from(step.bytes);
            }
        }
        Some(ProvisioningCost {
            run,
            requested,
            proxy_forwarded: at(StageId::ProxyForwarded),
            acknowledged: at(StageId::Acknowledged),
            expanded: at(StageId::Expanded),
            pre_linkage_ready: at(StageId::PreLinkageReady),
            shuffled: at(StageId::Shuffled),
            certified: at(StageId::Certified),
            batch_ready: at(StageId::BatchReady),
            downloaded: at(StageId::Downloaded),
            installed,
            bytes_by_transport: bytes,
            hops: self
                .run
                .kernel
                .steps
                .iter()
                .filter(|s| s.run == run)
                .count(),
        })
    }
}

/// What one provisioning run cost, stage by stage.
///
/// Every field is an instant, not a duration, so that a caller computing "how long did the
/// Registration Authority hold this" is subtracting two stamped instants rather than
/// trusting an interval this struct chose to name. The optional ones are optional because a
/// run that was refused — a blocklisted device — stamps `requested` and stops, and a
/// struct that pretended otherwise would report a zero where the truth is "never happened".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningCost {
    /// Which run.
    pub run: FlowRun,
    /// The device signed and sent its request.
    pub requested: SimTime,
    /// The Location Obscurer Proxy stripped the network identifiers.
    pub proxy_forwarded: Option<SimTime>,
    /// The Registration Authority acknowledged.
    pub acknowledged: Option<SimTime>,
    /// The RA finished the butterfly expansion.
    pub expanded: Option<SimTime>,
    /// Both Linkage Authorities had answered.
    pub pre_linkage_ready: Option<SimTime>,
    /// The shuffle window closed.
    pub shuffled: Option<SimTime>,
    /// The Pseudonym Certificate Authority certified the batch.
    pub certified: Option<SimTime>,
    /// The repository first held something downloadable.
    pub batch_ready: Option<SimTime>,
    /// The device downloaded its first batch.
    pub downloaded: Option<SimTime>,
    /// The device finished installing and can sign.
    pub installed: SimTime,
    /// Bytes on the wire, by transport.
    pub bytes_by_transport: BTreeMap<crate::net::Transport, u64>,
    /// How many hops the run took.
    pub hops: usize,
}

impl ProvisioningCost {
    /// Request to usable credential.
    pub const fn total(&self) -> Duration {
        Duration::between(self.requested, self.installed)
    }

    /// Bytes across every transport.
    pub fn bytes(&self) -> u64 {
        self.bytes_by_transport.values().sum()
    }

    /// The decomposition as `(name, instant)` pairs, in flow order, skipping the stages
    /// this run never reached.
    pub fn stages(&self) -> Vec<(&'static str, SimTime)> {
        let mut v = vec![(StageId::Requested.as_str(), self.requested)];
        for (id, t) in [
            (StageId::ProxyForwarded, self.proxy_forwarded),
            (StageId::Acknowledged, self.acknowledged),
            (StageId::Expanded, self.expanded),
            (StageId::PreLinkageReady, self.pre_linkage_ready),
            (StageId::Shuffled, self.shuffled),
            (StageId::Certified, self.certified),
            (StageId::BatchReady, self.batch_ready),
            (StageId::Downloaded, self.downloaded),
        ] {
            if let Some(t) = t {
                v.push((id.as_str(), t));
            }
        }
        v.push((StageId::Installed.as_str(), self.installed));
        v
    }
}

/// The revocation path's latency, stage by stage.
///
/// 05-protocols §8's table, assembled across the four flow runs it is spread over: the
/// report, the linkage resolution, the CRL issuance and one device's distribution. A Phase
/// 2 acceptance criterion is that this decomposition exists for any protocol, which is why
/// it is computed from [`StageStamp`]s by name rather than from anything SCMS-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationLatency {
    /// The device the decomposition is for.
    pub device: NodeId,
    /// Which path the CRL reached it by.
    pub transport: crate::net::Transport,
    /// `(stage, instant)` in 05-protocols §8's order, skipping stages this run never
    /// reached.
    pub stages: Vec<(StageId, SimTime)>,
    /// The size of the CRL that was distributed, bytes.
    pub crl_bytes: Option<u32>,
}

impl RevocationLatency {
    /// Assembles the decomposition from the four runs it is spread over.
    ///
    /// Takes the log and not the service, because this is a query over stamped stages and
    /// nothing else: the same function answers for any protocol whose flows stamp the
    /// 05-protocols §8 vocabulary, which is what makes the acceptance criterion
    /// protocol-independent rather than SCMS-shaped.
    pub fn assemble(
        log: &StageLog,
        device: NodeId,
        transport: crate::net::Transport,
        report: FlowRun,
        resolution: FlowRun,
        issuance: FlowRun,
        distribution: FlowRun,
    ) -> RevocationLatency {
        let order: [(StageId, FlowRun); 11] = [
            (StageId::Detect, report),
            (StageId::ReportSent, report),
            (StageId::Shuffled, report),
            (StageId::ReportReceived, report),
            (StageId::Decision, resolution),
            (StageId::Resolved, resolution),
            (StageId::Blocklisted, resolution),
            (StageId::Issued, issuance),
            (StageId::Published, issuance),
            (StageId::FirstRsuBroadcast, issuance),
            (StageId::Downloaded, distribution),
        ];
        let mut stages: Vec<(StageId, SimTime)> = order
            .into_iter()
            .filter_map(|(s, r)| log.at(r, s).map(|t| (s, t)))
            .collect();
        for s in [StageId::Processed, StageId::Enforced] {
            if let Some(t) = log.at_node(distribution, s, device) {
                stages.push((s, t));
            }
        }
        let crl_bytes = log
            .run(distribution)
            .into_iter()
            .find(|s| s.stage == StageId::Downloaded)
            .and_then(|s| s.size);
        RevocationLatency {
            device,
            transport,
            stages,
            crl_bytes,
        }
    }

    /// The instant a stage was reached.
    pub fn at(&self, stage: StageId) -> Option<SimTime> {
        self.stages
            .iter()
            .find(|(s, _)| *s == stage)
            .map(|(_, t)| *t)
    }

    /// The interval between two stages.
    pub fn between(&self, from: StageId, to: StageId) -> Option<Duration> {
        let (a, b) = (self.at(from)?, self.at(to)?);
        (b >= a).then(|| Duration::between(a, b))
    }

    /// Detection to enforcement: the headline number.
    pub fn total(&self) -> Option<Duration> {
        self.between(StageId::Detect, StageId::Enforced)
    }

    /// Each consecutive pair of stages and the time between them.
    ///
    /// Saturating, not subtracting: the stage order is 05-protocols §8's table order, and a
    /// protocol whose flows stamped two of those stages out of order would otherwise
    /// underflow here instead of reporting a zero-length step that a reader can see.
    pub fn steps(&self) -> Vec<(StageId, StageId, Duration)> {
        self.stages
            .windows(2)
            .map(|w| {
                (
                    w[0].0,
                    w[1].0,
                    Duration::from_nanos(w[1].1.saturating_sub(w[0].1)),
                )
            })
            .collect()
    }
}

/// The flows a device's start-up drives, for a caller that wants to name them.
pub const BOOTSTRAP_FLOWS: [FlowId; 2] = [FlowId::Enrolment, FlowId::Provisioning];
