//! The CAMP SCMS deployment: nine online entities, seven flows, one event kernel.
//!
//! Each backend role is a node with a service queue and links, never a function call,
//! because the question the simulator exists to answer is what provisioning and revocation
//! *cost* — in queueing delay, in round trips and in bytes. An investigation here is six
//! messages across four organisations, and its latency is the sum of their service times
//! and their link delays, exactly as 06-node-models §4 requires.
//!
//! The cryptography is the real thing from `v2xw-sec`: butterfly expansion, ECQV-shaped
//! key derivation and linkage values, with the arithmetic identity
//! `b'(i,j) = a + f₁(ck,(i,j)) + c` checked on the device at install time.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::hash::sha256;
use v2xw_core::ids::NodeId;
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{Duration, SimTime};
use v2xw_sec::butterfly::{self, Caterpillar};
use v2xw_sec::ec::{self, Point};
use v2xw_sec::linkage::{self, CrlLinkageEntry, LaId, LinkageSeed, LinkageValue, PreLinkageValue};
use v2xw_sec::primitive::{PrimitiveId, PrimitiveOpKind};

use crate::error::{ProtoError, Result};
use crate::kernel::{Delivery, Kernel, Outbox};
use crate::net::{BackendNet, Link, Transport};
use crate::pseudonym::{CertEvent, PseudonymStore, PseudonymStrategy};
use crate::scms::msg::{
    CertRequestItem, IssuedCredential, LaIndex, Lci, PcaLookup, PreLinkageBatch,
    ProvisioningRequest, ReportSubmission, ScmsMsg, ScmsSizes, SealedForPca,
};
use crate::scms::params::{ScmsNodes, ScmsParams};
use crate::sizes::{CertificateSizes, WireSize};
use crate::spec::{AES128_BLOCK, OpDescriptor};
use crate::stage::{FlowId, FlowRun, StageId};

const ECDSA: PrimitiveId = PrimitiveId::ECDSA_P256_SHA256;
const ECQV: PrimitiveId = PrimitiveId::ECQV_P256;
const SHA256: PrimitiveId = PrimitiveId::SHA_256;

/// What one device holds.
#[derive(Debug, Clone)]
pub struct DeviceState {
    /// Its node id.
    pub node: NodeId,
    /// Whether it has an enrolment certificate.
    pub enrolled: bool,
    /// Its butterfly caterpillar, once it has made a provisioning request.
    pub caterpillar: Option<Caterpillar>,
    /// The credentials it holds, by `(i, j)`.
    pub credentials: BTreeMap<(u32, u32), IssuedCredential>,
    /// The CRL entries it has processed.
    pub crl: Vec<CrlLinkageEntry>,
    /// Whether it has found itself on the CRL and stopped transmitting
    /// ([CAMP-EE §2.2.10.2 step 8.4]).
    pub silenced: bool,
    /// How many times it has polled for a batch that was not ready.
    pub download_retries: u32,
    /// Which of its pseudonyms is active, and the rule that changes it.
    pub store: PseudonymStore,
    /// The run of the provisioning flow in progress.
    provisioning: Option<ProvisioningProgress>,
}

#[derive(Debug, Clone, Copy)]
struct ProvisioningProgress {
    start_i: u32,
    periods: u32,
    downloaded: u32,
    first_batch_seen: bool,
}

impl DeviceState {
    /// A device that has done nothing yet.
    pub fn new(node: NodeId) -> DeviceState {
        DeviceState {
            node,
            enrolled: false,
            caterpillar: None,
            credentials: BTreeMap::new(),
            crl: Vec::new(),
            silenced: false,
            download_retries: 0,
            store: PseudonymStore::default(),
            provisioning: None,
        }
    }

    /// The `(i, j)` pairs this device could sign with at `now`, in ascending order.
    ///
    /// Usable means three things at once: downloaded, inside its validity window, and not
    /// matched by the CRL this device has processed. All three are the *device's* own
    /// view — the CRL it holds, not the CRL that exists — which is invariant I-P5.
    pub fn usable_at(&self, params: &ScmsParams, now: SimTime) -> Vec<(u32, u32)> {
        self.credentials
            .keys()
            .copied()
            .filter(|&(i, j)| {
                let (from, until) = params.validity(i);
                now >= from && now < until && !self.is_revoked(i, j)
            })
            .collect()
    }

    /// The credential the device would sign with now, if it has one.
    pub fn active_credential(&self) -> Option<&IssuedCredential> {
        self.store.active().and_then(|k| self.credentials.get(&k))
    }

    /// Whether the credential for `(i, j)` is revoked by the CRL this device holds.
    ///
    /// Forward-only by construction: [`CrlLinkageEntry::matches`] refuses a period before
    /// the entry's own, which is the backward-privacy property `tests/backward_privacy.rs`
    /// exercises end to end.
    pub fn is_revoked(&self, i: u32, j: u32) -> bool {
        self.credentials
            .get(&(i, j))
            .is_some_and(|c| self.crl.iter().any(|e| e.matches(i, j, c.lv)))
    }

    /// Every credential this device holds that its CRL revokes.
    pub fn revoked_credentials(&self) -> Vec<(u32, u32)> {
        self.credentials
            .keys()
            .copied()
            .filter(|&(i, j)| self.is_revoked(i, j))
            .collect()
    }

    /// Checks that a credential's derived private key matches the certified public key.
    ///
    /// `b'(i,j) = a + f₁(ck,(i,j)) + c` — the identity the whole butterfly construction
    /// rests on, and the difference between "the device downloaded bytes" and "the device
    /// has a usable certificate".
    pub fn credential_is_usable(&self, i: u32, j: u32) -> bool {
        let (Some(cat), Some(cred)) = (self.caterpillar.as_ref(), self.credentials.get(&(i, j)))
        else {
            return false;
        };
        let private = cat.signing_private(i, j, &cred.c);
        butterfly::derived_key_matches(&private, &cred.certified_public)
    }
}

/// The Registration Authority's state.
///
/// **What is not here is the point.** No linkage value, no pre-linkage value, no seed and
/// no certified public key: the RA holds enrolment records, request hashes, chain
/// identifiers and sealed blobs it cannot open. `tests/privacy.rs` asserts that by walking
/// this struct.
#[derive(Debug, Default)]
pub struct RaState {
    /// Devices with an enrolment certificate.
    pub enrolled: BTreeSet<NodeId>,
    /// Blocklisted enrolment certificates (the passive half of revocation).
    pub blocklist: BTreeSet<NodeId>,
    /// Request hash → device, and the chains the LAs allocated.
    pub requests: BTreeMap<[u8; 32], RequestRecord>,
    /// Requests refused because the device is blocklisted.
    pub refused: u32,
    /// Provisioning jobs waiting for the shuffle window.
    jobs: Vec<ProvisioningJob>,
    /// Reports waiting for the report shuffle window.
    reports: Vec<(FlowRun, ReportSubmission)>,
    /// Batches ready for download: device → i → credentials.
    repos: BTreeMap<NodeId, BTreeMap<u32, Vec<IssuedCredential>>>,
    shuffle_armed: bool,
    report_shuffle_armed: bool,
}

/// What the RA records about one provisioning request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestRecord {
    /// The device that made it.
    pub device: NodeId,
    /// LA1's chain, once LA1 has allocated one.
    pub lci1: Option<Lci>,
    /// LA2's chain.
    pub lci2: Option<Lci>,
}

#[derive(Debug)]
struct ProvisioningJob {
    run: FlowRun,
    flow: FlowId,
    device: NodeId,
    request_hash: [u8; 32],
    start_i: u32,
    periods: u32,
    jmax: u32,
    cocoons: BTreeMap<(u32, u32), Point>,
    plv: [BTreeMap<(u32, u32), SealedForPca<PreLinkageValue>>; 2],
    lci: [Option<Lci>; 2],
    responses: u8,
    periods_certified: u32,
    sent: bool,
    batch_ready_stamped: bool,
}

/// The Pseudonym Certificate Authority's state ([BRECHT Table II]).
#[derive(Debug, Default)]
pub struct PcaState {
    /// `(i, linkage value)` → what the PCA stored when it issued that certificate.
    pub issued: BTreeMap<(u32, [u8; 9]), PcaLookup>,
    /// How many certificates it has issued.
    pub issued_count: u64,
    certified_runs: BTreeSet<FlowRun>,
}

/// One Linkage Authority's state.
#[derive(Debug)]
pub struct LaState {
    /// Which LA this is.
    pub index: LaIndex,
    /// Its identifier, as it appears in a CRL entry.
    pub la_id: LaId,
    /// Chain → initial seed. The seed never leaves this map except as `ls(i)` for a
    /// revocation period ([BRECHT §VI-D]).
    pub chains: BTreeMap<Lci, LinkageSeed>,
    /// Chain → the device it belongs to. The LA knows this and nothing else about the
    /// device: not its keys, not its certificates, not its linkage values.
    pub owner: BTreeMap<Lci, NodeId>,
    next_lci: u64,
}

impl LaState {
    fn new(index: LaIndex, la_id: LaId) -> LaState {
        LaState {
            index,
            la_id,
            chains: BTreeMap::new(),
            owner: BTreeMap::new(),
            next_lci: 1,
        }
    }
}

/// The Misbehaviour Authority's state.
#[derive(Debug, Default)]
pub struct MaState {
    /// Reports it has received, in arrival order.
    pub reports: Vec<ReportSubmission>,
    /// The investigation in progress, if any.
    pub case: Option<Case>,
    /// Devices it has concluded should be revoked, by request hash.
    pub decisions: Vec<[u8; 32]>,
}

/// One investigation.
#[derive(Debug, Clone)]
pub struct Case {
    /// The flow run of the resolution.
    pub run: FlowRun,
    /// The flow run reserved for the CRL issuance that follows it.
    pub crl_run: FlowRun,
    /// The two reports being correlated.
    pub subjects: [(u32, LinkageValue); 2],
    /// What the PCA answered, in order.
    pub lookups: Vec<PcaLookup>,
    /// What the two LAs answered.
    pub same: [Option<bool>; 2],
    /// The seeds the LAs released.
    pub seeds: [Option<(LaId, LinkageSeed)>; 2],
    /// The first revoked period.
    pub i_rev: u32,
    /// Certificates per period, for the CRL entry.
    pub jmax: u32,
    /// Whether the resolution succeeded.
    pub resolved: bool,
}

/// The CRL Generator's and the CRL Store's state.
#[derive(Debug, Default)]
pub struct CrlState {
    /// The published entries.
    pub entries: Vec<CrlLinkageEntry>,
}

/// Everything the deployment knows.
pub struct ScmsState {
    /// Which node hosts which role.
    pub nodes: ScmsNodes,
    /// The protocol's parameters.
    pub params: ScmsParams,
    /// The size model.
    pub sizes: ScmsSizes,
    /// The Registration Authority.
    pub ra: RaState,
    /// The Pseudonym Certificate Authority.
    pub pca: PcaState,
    /// The two Linkage Authorities.
    pub la: [LaState; 2],
    /// The Misbehaviour Authority.
    pub ma: MaState,
    /// What the CRL Generator has assembled.
    pub crlg: CrlState,
    /// What the CRL Store holds.
    pub crl_store: CrlState,
    /// What the broadcast path has carried.
    pub crl_broadcast: CrlState,
    /// The devices.
    pub devices: BTreeMap<NodeId, DeviceState>,
    /// The deterministic streams.
    pub rng: RngRegistry,
    /// Which devices the Enrolment CA has issued a certificate to.
    ///
    /// The simulator's bookkeeping, not an entity's knowledge: the RA learns a device is
    /// enrolled by verifying the signature on its provisioning request, which is charged
    /// where that happens.
    pub issued_enrolment: BTreeSet<NodeId>,
    next_run: u32,
}

/// A deployment and the kernel it runs on.
pub struct ScmsRun {
    /// The entities.
    pub state: ScmsState,
    /// The schedule, the queues and the logs.
    pub kernel: Kernel<ScmsMsg>,
}

impl ScmsRun {
    /// Builds the reference topology: every online role on its own node, connected to the
    /// entities it talks to.
    ///
    /// # Errors
    /// [`ProtoError::Size`] if the certificate profile does not encode.
    pub fn new(params: ScmsParams) -> Result<ScmsRun> {
        ScmsRun::new_at(params, 0)
    }

    /// The same deployment with its clock starting at `t0`.
    ///
    /// The engine's clock does not start at zero, and a provisioning flow that began at
    /// the scenario's `t0` must stamp its stages on the engine's timeline rather than on
    /// one of its own. `t0` is supplied by the caller; nothing here reads a clock.
    ///
    /// # Errors
    /// [`ProtoError::Size`] if the certificate profile does not encode.
    pub fn new_at(params: ScmsParams, t0: SimTime) -> Result<ScmsRun> {
        let nodes = ScmsNodes::default();
        let sizes = ScmsSizes::new(CertificateSizes::measured()?, params.sizes);
        let mut net = BackendNet::new();
        let backend = Link {
            latency: params.backend_link_latency,
            bandwidth_bps: params.backend_link_bandwidth_bps,
            transport: Transport::BackendNet,
        };
        for (a, b) in nodes.backend_links() {
            net.connect(a, b, backend);
        }
        let mut kernel = Kernel::new_at(net, t0);
        for (node, spec) in nodes.backend_service_models(&params) {
            kernel.host(node, &spec, params.backend_profile);
        }
        Ok(ScmsRun {
            state: ScmsState {
                nodes,
                params,
                sizes,
                ra: RaState::default(),
                pca: PcaState::default(),
                la: [
                    LaState::new(LaIndex::One, LaId(1)),
                    LaState::new(LaIndex::Two, LaId(2)),
                ],
                ma: MaState::default(),
                crlg: CrlState::default(),
                crl_store: CrlState::default(),
                crl_broadcast: CrlState::default(),
                devices: BTreeMap::new(),
                rng: RngRegistry::new(params.master_seed),
                issued_enrolment: BTreeSet::new(),
                next_run: 0,
            },
            kernel,
        })
    }

    /// Adds a device, with its uplink to the privacy proxy and to the CRL store.
    pub fn add_device(&mut self, node: NodeId) {
        let p = &self.state.params;
        let uu = Link {
            latency: p.uu_link_latency,
            bandwidth_bps: p.uu_link_bandwidth_bps,
            transport: Transport::CellularUu,
        };
        let n = self.state.nodes;
        {
            let net = self.kernel.net_mut();
            for peer in [n.lop, n.ra, n.dcm, n.eca, n.crl_store] {
                net.connect(node, peer, uu);
            }
        }
        self.kernel.host(
            node,
            &crate::service::ServiceModelSpec::new(1, p.device_overhead),
            p.device_profile,
        );
        self.state.devices.insert(node, DeviceState::new(node));
    }

    /// Puts `device` in range of the roadside CRL broadcast path.
    ///
    /// The second distribution path of 05-protocols §3.2: the same signed CRL, over the
    /// 5.9 GHz air interface instead of the cellular uplink. The two differ in exactly one
    /// modelled thing — the link — which is what makes "how long until an RSU-only vehicle
    /// enforces it" a different number from "how long until a connected one does".
    ///
    /// The air interface itself belongs to `v2xw-radio`: this is a point-to-point link
    /// with the OFDM rate as its bandwidth, so a 400 kB CRL takes the 533 ms it takes at
    /// 6 Mbit/s. Contention, fragmentation and the loss process are the radio crate's, and
    /// a scenario that needs them drives the broadcast through the engine's PHY instead.
    pub fn attach_rsu(&mut self, device: NodeId) {
        let p = &self.state.params;
        let air = Link {
            latency: p.v2x_air_latency,
            bandwidth_bps: p.v2x_air_bandwidth_bps,
            transport: Transport::V2xAir,
        };
        let broadcast = self.state.nodes.crl_broadcast;
        self.kernel.net_mut().connect(device, broadcast, air);
    }

    /// Sets the pseudonym-change rule `device` follows.
    pub fn set_strategy(&mut self, device: NodeId, strategy: PseudonymStrategy) {
        if let Some(d) = self.state.devices.get_mut(&device) {
            d.store.set_strategy(strategy);
        }
    }

    /// Adds travel since the last pseudonym change, for the `distance` rule.
    pub fn travelled_cm(&mut self, device: NodeId, cm: u64) {
        if let Some(d) = self.state.devices.get_mut(&device) {
            d.store.travelled_cm(cm);
        }
    }

    /// Tells `device` it has left a mix zone, for the `mix-zone` rule.
    pub fn left_mix_zone(&mut self, device: NodeId) {
        if let Some(d) = self.state.devices.get_mut(&device) {
            d.store.left_mix_zone();
        }
    }

    /// Rotates `device`'s pseudonym if its rule says one is due at `now`.
    ///
    /// Returns the `sec.cert` record for the change, or `None` if none happened. The
    /// device's own store is what swaps: [`DeviceState::active_credential`] returns a
    /// different credential afterwards, with a different linkage value, which is the only
    /// identifier a receiver of its next message would see.
    pub fn rotate(&mut self, device: NodeId, now: SimTime) -> Option<CertEvent> {
        let params = self.state.params;
        let dev = self.state.devices.get_mut(&device)?;
        let usable = dev.usable_at(&params, now);
        let active_revoked = dev
            .store
            .active()
            .is_some_and(|(i, j)| dev.is_revoked(i, j));
        let (reason, next) = dev.store.rotate(now, &usable, active_revoked)?;
        let (i, j) = next.unwrap_or((0, 0));
        let lv = next
            .and_then(|k| dev.credentials.get(&k))
            .map_or([0u8; 9], |c| *c.lv.as_bytes());
        Some(CertEvent {
            t: now,
            node: device,
            event: "change",
            reason: Some(reason),
            i_period: i,
            j_index: j,
            linkage_value: lv,
            changes: dev.store.changes(),
        })
    }

    fn new_run(&mut self) -> FlowRun {
        self.state.next_run += 1;
        FlowRun(self.state.next_run)
    }

    fn inject(&mut self, to: NodeId, msg: ScmsMsg, flow: FlowId, run: FlowRun) {
        let at = self.kernel.now();
        self.inject_at(at, to, msg, flow, run);
    }

    /// Schedules a flow's first message at `at`, never earlier than the kernel's clock.
    ///
    /// A device that spawns 1.5 s into a run asks for credentials at 1.5 s. Clamping to
    /// the clock matters: the heap is ordered by `(time, sequence)`, and an injection in
    /// the past would be dispatched before deliveries already in flight, which is a
    /// causality violation rather than an early message.
    fn inject_at(&mut self, at: SimTime, to: NodeId, msg: ScmsMsg, flow: FlowId, run: FlowRun) {
        self.kernel.inject_at(
            at,
            Delivery {
                at,
                from: to,
                to,
                msg,
                flow,
                run,
            },
        );
    }

    /// Starts the enrolment flow for a device.
    pub fn enrol(&mut self, device: NodeId) -> FlowRun {
        let at = self.kernel.now();
        self.enrol_at(device, at)
    }

    /// Starts the enrolment flow at `at`.
    pub fn enrol_at(&mut self, device: NodeId, at: SimTime) -> FlowRun {
        let run = self.new_run();
        self.inject_at(
            at,
            device,
            ScmsMsg::EnrolRequest { device },
            FlowId::Enrolment,
            run,
        );
        run
    }

    /// Starts the butterfly provisioning flow.
    pub fn provision(&mut self, device: NodeId, start_i: u32, periods: u32, jmax: u32) -> FlowRun {
        let at = self.kernel.now();
        self.provision_at(device, at, start_i, periods, jmax)
    }

    /// Starts the butterfly provisioning flow at `at`.
    pub fn provision_at(
        &mut self,
        device: NodeId,
        at: SimTime,
        start_i: u32,
        periods: u32,
        jmax: u32,
    ) -> FlowRun {
        self.start_provisioning(device, at, start_i, periods, jmax, FlowId::Provisioning)
    }

    fn start_provisioning(
        &mut self,
        device: NodeId,
        at: SimTime,
        start_i: u32,
        periods: u32,
        jmax: u32,
        flow: FlowId,
    ) -> FlowRun {
        let run = self.new_run();
        let mut seed = [0u8; butterfly::SEED_BYTES];
        {
            let mut g = self
                .state
                .rng
                .checkout(RngDomain::Crypto, EntityRef::Node(device));
            g.fill_bytes(&mut seed);
        }
        let cat = Caterpillar::from_seed(&seed).expect("64 bytes of seed");
        let request = ProvisioningRequest {
            device,
            signing_public: cat.signing_public(),
            encryption_public: cat.encryption_public(),
            ck: *cat.ck(),
            ek: *cat.ek(),
            start_i,
            periods,
            jmax,
        };
        if let Some(d) = self.state.devices.get_mut(&device) {
            d.caterpillar = Some(cat);
            d.download_retries = 0;
            d.provisioning = Some(ProvisioningProgress {
                start_i,
                periods,
                downloaded: 0,
                first_batch_seen: false,
            });
        }
        self.inject_at(
            at,
            device,
            ScmsMsg::ProvisioningRequest(Box::new(request)),
            flow,
            run,
        );
        run
    }

    /// Starts a top-up: one more i-period on an existing caterpillar.
    ///
    /// The same flow with a different name, which is what 05-protocols §3.2 says top-up is
    /// — "RA pre-generates up to 3 years ahead and adds a week every week".
    pub fn topup(&mut self, device: NodeId, i: u32, jmax: u32) -> FlowRun {
        let at = self.kernel.now();
        self.start_provisioning(device, at, i, 1, jmax, FlowId::Topup)
    }

    /// Submits a misbehaviour report about the certificate `(subject_i, subject_lv)`.
    pub fn submit_report(
        &mut self,
        reporter: NodeId,
        subject_i: u32,
        subject_lv: LinkageValue,
    ) -> FlowRun {
        let run = self.new_run();
        let observed_at = self.kernel.now();
        self.inject(
            reporter,
            ScmsMsg::Report(Box::new(ReportSubmission {
                reporter,
                subject_i,
                subject_lv,
                observed_at,
            })),
            FlowId::Report,
            run,
        );
        run
    }

    /// Opens an investigation over the two reports the MA holds at `indices`, revoking
    /// from period `i_rev` forward if they resolve to one device.
    ///
    /// Returns the resolution run and the CRL-issuance run.
    pub fn investigate(
        &mut self,
        a: usize,
        b: usize,
        i_rev: u32,
        jmax: u32,
    ) -> Option<(FlowRun, FlowRun)> {
        let run = self.new_run();
        let crl_run = self.new_run();
        let (ra, rb) = {
            let reports = &self.state.ma.reports;
            (reports.get(a)?.clone(), reports.get(b)?.clone())
        };
        self.state.ma.case = Some(Case {
            run,
            crl_run,
            subjects: [(ra.subject_i, ra.subject_lv), (rb.subject_i, rb.subject_lv)],
            lookups: Vec::new(),
            same: [None, None],
            seeds: [None, None],
            i_rev,
            jmax,
            resolved: false,
        });
        let ma = self.state.nodes.ma;
        self.inject(
            ma,
            ScmsMsg::PcaLookupRequest {
                i: ra.subject_i,
                lv: ra.subject_lv,
            },
            FlowId::LinkageResolution,
            run,
        );
        Some((run, crl_run))
    }

    /// Has a device fetch, expand and enforce the current CRL.
    pub fn distribute_crl(&mut self, device: NodeId) -> FlowRun {
        let run = self.new_run();
        self.inject(
            device,
            ScmsMsg::CrlDownloadRequest { device },
            FlowId::CrlDistribution,
            run,
        );
        run
    }

    /// Has the roadside broadcast path push the current CRL to `device`.
    ///
    /// The device must have been attached with [`ScmsRun::attach_rsu`]; without a link the
    /// kernel refuses to deliver rather than delivering for free (invariant I-P1).
    pub fn broadcast_crl_to(&mut self, device: NodeId) -> FlowRun {
        let run = self.new_run();
        let broadcast = self.state.nodes.crl_broadcast;
        self.inject(
            broadcast,
            ScmsMsg::CrlAirBroadcast { device },
            FlowId::CrlDistribution,
            run,
        );
        run
    }

    /// Runs every delivery due at or before `horizon`, then stops.
    ///
    /// How the engine drives the deployment: it advances its own clock to `t`, calls this,
    /// and the backend does exactly the work that was due by then. What is still in flight
    /// stays in flight, which is the difference between a provisioning round trip that
    /// costs simulated time and one that completes inside a single engine step.
    ///
    /// # Errors
    /// Whatever [`Kernel::dispatch`] or a handler returns.
    pub fn run_until(&mut self, horizon: SimTime) -> Result<()> {
        let ScmsRun { state, kernel } = self;
        while let Some(d) = kernel.next_delivery_before(horizon) {
            let profile = kernel.profile_of(d.to);
            let mut out = Outbox::new(profile);
            let (at, to) = (d.at, d.to);
            state.handle(d, &mut out)?;
            kernel.dispatch(at, to, out)?;
        }
        Ok(())
    }

    /// The single entry on the published CRL, for a test that needs to inspect it.
    pub fn crl_entry(&self) -> Option<&v2xw_sec::linkage::CrlLinkageEntry> {
        self.state.crl_store.entries.first()
    }

    /// Runs until nothing is scheduled.
    ///
    /// # Errors
    /// Whatever [`Kernel::dispatch`] or a handler returns: an unhosted node, a missing
    /// link or a flow driven out of order — all modelling defects.
    pub fn run(&mut self) -> Result<()> {
        let ScmsRun { state, kernel } = self;
        while let Some(d) = kernel.next_delivery() {
            let profile = kernel.profile_of(d.to);
            let mut out = Outbox::new(profile);
            let (at, to) = (d.at, d.to);
            state.handle(d, &mut out)?;
            kernel.dispatch(at, to, out)?;
        }
        Ok(())
    }
}

impl ScmsState {
    fn sign(out: &mut Outbox<ScmsMsg>, n: u32) {
        out.compute(OpDescriptor::new(ECDSA, PrimitiveOpKind::Sign, n));
    }

    fn verify(out: &mut Outbox<ScmsMsg>, n: u32) {
        out.compute(OpDescriptor::new(ECDSA, PrimitiveOpKind::Verify, n));
    }

    /// One elliptic-curve scalar multiplication, charged against the ECQV descriptor,
    /// whose declared cost proxy in `v2xw-sec` is the ECDSA P-256 anchor precisely because
    /// a reconstruction is "one point multiplication plus one addition".
    fn scalar_mults(out: &mut Outbox<ScmsMsg>, n: u32) {
        out.compute(OpDescriptor::new(ECQV, PrimitiveOpKind::Sign, n));
    }

    /// AES-128 block operations. No profile in 04-models §9.4 publishes an AES anchor, so
    /// these are **counted and charged zero time** rather than given an invented rate; the
    /// count is what the linkage-expansion cost metric reports.
    fn aes(out: &mut Outbox<ScmsMsg>, n: u32) {
        out.compute(OpDescriptor::new(AES128_BLOCK, PrimitiveOpKind::Sign, n));
    }

    /// SHA-256 compressions, counted for the same reason and with the same caveat.
    fn hashes(out: &mut Outbox<ScmsMsg>, n: u32) {
        out.compute(OpDescriptor::new(SHA256, PrimitiveOpKind::Verify, n));
    }

    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, d: Delivery<ScmsMsg>, out: &mut Outbox<ScmsMsg>) -> Result<()> {
        let n = self.nodes;
        let (flow, run, to, at, from) = (d.flow, d.run, d.to, d.at, d.from);
        match d.msg {
            // ---------------- enrolment ----------------
            ScmsMsg::EnrolRequest { device } if to == device => {
                Self::sign(out, 1);
                out.stage_at(StageId::Requested, device, None, flow, run);
                out.send(
                    n.dcm,
                    ScmsMsg::EnrolRequest { device },
                    "enrol-request",
                    self.sizes.enrol_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::EnrolRequest { device } => {
                // Device Configuration Manager: checks the device is one it configured,
                // re-signs and forwards to the Enrolment CA.
                Self::verify(out, 1);
                Self::sign(out, 1);
                out.stage_at(StageId::ProxyForwarded, to, None, flow, run);
                out.send(
                    n.eca,
                    ScmsMsg::EnrolForward { device },
                    "enrol-forward",
                    self.sizes.enrol_forward(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::EnrolForward { device } => {
                Self::verify(out, 1);
                Self::sign(out, 1);
                self.issued_enrolment.insert(device);
                out.stage_at(StageId::Certified, to, None, flow, run);
                out.send(
                    device,
                    ScmsMsg::EnrolResponse { device },
                    "enrol-response",
                    self.sizes.enrol_response(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::EnrolResponse { device } => {
                Self::verify(out, 2);
                if let Some(dev) = self.devices.get_mut(&device) {
                    dev.enrolled = true;
                }
                out.stage_at(StageId::Installed, device, None, flow, run);
            }

            // ---------------- provisioning ----------------
            ScmsMsg::ProvisioningRequest(req) if to == req.device => {
                // The device signs the request with its enrolment certificate and
                // encrypts it to the RA.
                Self::sign(out, 1);
                Self::scalar_mults(out, 1);
                out.stage_at(StageId::Requested, req.device, None, flow, run);
                out.send(
                    n.lop,
                    ScmsMsg::ProvisioningRequest(req),
                    "provisioning-request",
                    self.sizes.provisioning_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::ProvisioningRequest(req) if to == n.lop => {
                // The Location Obscurer Proxy strips the network identifiers. It performs
                // no cryptography: the payload is encrypted to the RA and it cannot read
                // it, which is the whole reason it is a separate organisation.
                out.stage_at(StageId::ProxyForwarded, to, None, flow, run);
                out.send(
                    n.ra,
                    ScmsMsg::ProvisioningRequest(req),
                    "provisioning-request-proxied",
                    self.sizes.provisioning_request(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::ProvisioningRequest(req) => {
                Self::verify(out, 1);
                if self.ra.blocklist.contains(&req.device) {
                    // The passive half of revocation: the RA simply stops issuing.
                    self.ra.refused += 1;
                    return Ok(());
                }
                self.ra.enrolled.insert(req.device);
                let request_hash = request_hash(&req);
                self.ra.requests.insert(
                    request_hash,
                    RequestRecord {
                        device: req.device,
                        lci1: None,
                        lci2: None,
                    },
                );
                out.stage_at(StageId::Acknowledged, to, None, flow, run);

                // Butterfly expansion: two scalar multiplications per certificate, and
                // nothing that reveals the certified key, which only the PCA's `c` fixes.
                let total = req.periods.saturating_mul(req.jmax);
                Self::scalar_mults(out, 2 * total);
                let mut cocoons = BTreeMap::new();
                for i in req.start_i..req.start_i + req.periods {
                    for j in 0..req.jmax {
                        let (b, _q) = butterfly::ra_cocoon_keys(
                            &req.signing_public,
                            &req.encryption_public,
                            &req.ck,
                            &req.ek,
                            i,
                            j,
                        );
                        cocoons.insert((i, j), b);
                    }
                }
                out.stage_at(StageId::Expanded, to, None, flow, run);

                self.ra.jobs.push(ProvisioningJob {
                    run,
                    flow,
                    device: req.device,
                    request_hash,
                    start_i: req.start_i,
                    periods: req.periods,
                    jmax: req.jmax,
                    cocoons,
                    plv: [BTreeMap::new(), BTreeMap::new()],
                    lci: [None, None],
                    responses: 0,
                    periods_certified: 0,
                    sent: false,
                    batch_ready_stamped: false,
                });

                out.send(
                    req.device,
                    ScmsMsg::ProvisioningAck {
                        device: req.device,
                        request_hash,
                    },
                    "provisioning-ack",
                    self.sizes.provisioning_ack(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
                for la in [LaIndex::One, LaIndex::Two] {
                    out.send(
                        self.nodes.la(la),
                        ScmsMsg::PreLinkageRequest {
                            device: req.device,
                            la,
                            start_i: req.start_i,
                            periods: req.periods,
                            jmax: req.jmax,
                        },
                        "pre-linkage-request",
                        self.sizes.pre_linkage_request(),
                        Transport::BackendNet,
                        flow,
                        run,
                    );
                }
            }
            ScmsMsg::ProvisioningAck { device, .. } => {
                // The device now waits for the first batch time and polls the repository.
                out.start_timer(
                    self.params.first_batch_delay,
                    ScmsMsg::BatchDownloadRequest {
                        device,
                        i: self
                            .devices
                            .get(&device)
                            .and_then(|d| d.provisioning)
                            .map_or(0, |p| p.start_i),
                    },
                    flow,
                    run,
                );
            }
            ScmsMsg::PreLinkageRequest {
                device,
                la,
                start_i,
                periods,
                jmax,
            } => {
                let idx = la.idx();
                let lci = {
                    let st = &mut self.la[idx];
                    let lci = Lci(st.next_lci);
                    st.next_lci += 1;
                    lci
                };
                let mut seed_bytes = [0u8; linkage::LS_BYTES];
                {
                    let mut g = self
                        .rng
                        .checkout(RngDomain::Crypto, EntityRef::Node(self.nodes.la(la)));
                    g.fill_bytes(&mut seed_bytes);
                }
                let ls0 = LinkageSeed::new(seed_bytes);
                {
                    let st = &mut self.la[idx];
                    st.chains.insert(lci, ls0);
                    st.owner.insert(lci, device);
                }
                let la_id = self.la[idx].la_id;

                // One hash per period to walk the chain, two AES blocks per pre-linkage
                // value ([BRECHT §V-B]).
                Self::hashes(out, periods);
                Self::aes(out, periods.saturating_mul(jmax).saturating_mul(2));

                let mut values = Vec::new();
                for i in start_i..start_i + periods {
                    let ls_i = linkage::linkage_seed_at(la_id, ls0, i);
                    for j in 0..jmax {
                        values.push((
                            i,
                            j,
                            SealedForPca::seal(linkage::pre_linkage_value(la_id, ls_i, j)),
                        ));
                    }
                }
                let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
                out.send(
                    n.ra,
                    ScmsMsg::PreLinkageResponse(Box::new(PreLinkageBatch {
                        la,
                        la_id,
                        lci,
                        values,
                    })),
                    "pre-linkage-response",
                    self.sizes.pre_linkage_response(count),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::PreLinkageResponse(batch) => {
                let Some(job) = self.ra.jobs.iter_mut().find(|j| j.run == run) else {
                    return Err(ProtoError::Flow {
                        flow: "provisioning",
                        detail: "pre-linkage values for a request the RA does not hold".into(),
                    });
                };
                let idx = batch.la.idx();
                job.lci[idx] = Some(batch.lci);
                for (i, j, plv) in batch.values {
                    job.plv[idx].insert((i, j), plv);
                }
                job.responses += 1;
                let hash = job.request_hash;
                let ready = job.responses == 2;
                let lcis = job.lci;
                if let Some(rec) = self.ra.requests.get_mut(&hash) {
                    rec.lci1 = lcis[0];
                    rec.lci2 = lcis[1];
                }
                if ready {
                    out.stage_at(StageId::PreLinkageReady, to, None, flow, run);
                    if !self.ra.shuffle_armed {
                        self.ra.shuffle_armed = true;
                        out.start_timer(
                            self.params.shuffle_window,
                            ScmsMsg::ShuffleTimer,
                            flow,
                            run,
                        );
                    }
                }
            }
            ScmsMsg::ShuffleTimer => {
                self.ra.shuffle_armed = false;
                let jobs = core::mem::take(&mut self.ra.jobs);
                let mut still_waiting = Vec::new();
                for mut job in jobs {
                    if job.responses < 2 || job.sent {
                        still_waiting.push(job);
                        continue;
                    }
                    job.sent = true;
                    out.stage_at(StageId::Shuffled, to, None, job.flow, job.run);
                    for i in job.start_i..job.start_i + job.periods {
                        let mut items = Vec::new();
                        for j in 0..job.jmax {
                            let (Some(b), Some(p1), Some(p2), Some(l1), Some(l2)) = (
                                job.cocoons.get(&(i, j)).copied(),
                                job.plv[0].get(&(i, j)).cloned(),
                                job.plv[1].get(&(i, j)).cloned(),
                                job.lci[0],
                                job.lci[1],
                            ) else {
                                continue;
                            };
                            items.push(CertRequestItem {
                                i,
                                j,
                                cocoon_signing: b,
                                plv1: p1,
                                plv2: p2,
                                lci1: SealedForPca::seal(l1),
                                lci2: SealedForPca::seal(l2),
                                request_hash: job.request_hash,
                            });
                        }
                        let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
                        out.send(
                            n.pca,
                            ScmsMsg::CertRequest {
                                device: job.device,
                                i,
                                items: Box::new(items),
                            },
                            "cert-request",
                            self.sizes.cert_request(count),
                            Transport::BackendNet,
                            job.flow,
                            job.run,
                        );
                    }
                    self.ra.repos.entry(job.device).or_default();
                    still_waiting.push(job);
                }
                self.ra.jobs = still_waiting;
            }
            ScmsMsg::CertRequest { device, i, items } => {
                let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
                // Per certificate: two ECIES decryptions of the pre-linkage values, one
                // ECQV issuance, one ECIES encryption and one signature (05-protocols §3.2).
                Self::scalar_mults(out, 4 * count);
                Self::sign(out, count);
                let mut credentials = Vec::new();
                for item in items.into_iter() {
                    let (Some(plv1), Some(plv2), Some(lci1), Some(lci2)) = (
                        item.plv1.open(to, n.pca),
                        item.plv2.open(to, n.pca),
                        item.lci1.open(to, n.pca),
                        item.lci2.open(to, n.pca),
                    ) else {
                        return Err(ProtoError::Flow {
                            flow: "provisioning",
                            detail: "only the PCA may open a sealed pre-linkage value".into(),
                        });
                    };
                    let lv = linkage::linkage_value(plv1, plv2);
                    let mut be = [0u8; 32];
                    {
                        let mut g = self.rng.checkout(RngDomain::Crypto, EntityRef::Node(n.pca));
                        g.fill_bytes(&mut be);
                    }
                    let c = ec::scalar_from_be_mod_n(&be);
                    let (certified_public, _big_c) =
                        butterfly::pca_certify_explicit(&item.cocoon_signing, &c);
                    self.pca.issued.insert(
                        (item.i, *lv.as_bytes()),
                        PcaLookup {
                            lci1,
                            lci2,
                            request_hash: item.request_hash,
                        },
                    );
                    self.pca.issued_count += 1;
                    credentials.push(IssuedCredential {
                        i: item.i,
                        j: item.j,
                        lv,
                        certified_public,
                        c,
                    });
                }
                if self.pca.certified_runs.insert(run) {
                    out.stage_at(StageId::Certified, to, None, flow, run);
                }
                out.send(
                    n.ra,
                    ScmsMsg::CertResponse {
                        device,
                        i,
                        credentials: Box::new(credentials),
                    },
                    "cert-response",
                    self.sizes.cert_batch(count),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::CertResponse {
                device,
                i,
                credentials,
            } => {
                self.ra
                    .repos
                    .entry(device)
                    .or_default()
                    .insert(i, *credentials);
                // `batch_ready` is the first moment the repository holds something the
                // device can fetch, not the last: a device that polls early downloads the
                // first i-period while later ones are still being certified, and a stage
                // that came after that download would be a lie about the order.
                let mut first = false;
                let mut finished = false;
                if let Some(j) = self.ra.jobs.iter_mut().find(|j| j.run == run) {
                    j.periods_certified += 1;
                    first = !core::mem::replace(&mut j.batch_ready_stamped, true);
                    finished = j.periods_certified >= j.periods;
                }
                if first {
                    out.stage_at(StageId::BatchReady, to, None, flow, run);
                }
                if finished {
                    self.ra.jobs.retain(|j| j.run != run);
                }
            }
            ScmsMsg::BatchDownloadRequest { device, i } if to == device => {
                out.send(
                    n.ra,
                    ScmsMsg::BatchDownloadRequest { device, i },
                    "batch-download-request",
                    self.sizes.batch_download_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::BatchDownloadRequest { device, i } => {
                let credentials = self
                    .ra
                    .repos
                    .get(&device)
                    .and_then(|r| r.get(&i))
                    .cloned()
                    .unwrap_or_default();
                let count = u32::try_from(credentials.len()).unwrap_or(u32::MAX);
                out.send(
                    device,
                    ScmsMsg::BatchDownload {
                        device,
                        i,
                        credentials: Box::new(credentials),
                    },
                    "batch-download",
                    self.sizes.batch_download(count),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::BatchDownload {
                device,
                i,
                credentials,
            } => {
                let poll = self.params.download_poll_interval;
                let cap = self.params.max_download_polls;
                let Some(dev) = self.devices.get_mut(&device) else {
                    return Err(ProtoError::NoEntity { node: device });
                };
                if credentials.is_empty() {
                    dev.download_retries += 1;
                    if dev.download_retries <= cap {
                        out.start_timer(
                            poll,
                            ScmsMsg::BatchDownloadRequest { device, i },
                            flow,
                            run,
                        );
                    }
                    return Ok(());
                }
                // One ECQV reconstruction per certificate on download (05-protocols §3.2).
                let count = u32::try_from(credentials.len()).unwrap_or(u32::MAX);
                out.compute(OpDescriptor::new(ECQV, PrimitiveOpKind::Verify, count));
                for cred in credentials.into_iter() {
                    dev.credentials.insert((cred.i, cred.j), cred);
                }
                let mut finished = false;
                let mut next_i = None;
                if let Some(p) = dev.provisioning.as_mut() {
                    p.downloaded += 1;
                    if !p.first_batch_seen {
                        p.first_batch_seen = true;
                        out.stage_at(StageId::Downloaded, device, None, flow, run);
                    }
                    if p.downloaded >= p.periods {
                        finished = true;
                    } else {
                        next_i = Some(p.start_i + p.downloaded);
                    }
                }
                if finished {
                    dev.provisioning = None;
                    out.stage_at(StageId::Installed, device, None, flow, run);
                } else if let Some(i_next) = next_i {
                    out.send(
                        n.ra,
                        ScmsMsg::BatchDownloadRequest { device, i: i_next },
                        "batch-download-request",
                        self.sizes.batch_download_request(),
                        Transport::CellularUu,
                        flow,
                        run,
                    );
                }
            }

            // ---------------- misbehaviour reporting ----------------
            ScmsMsg::Report(r) if to == r.reporter => {
                Self::sign(out, 1);
                Self::scalar_mults(out, 1);
                out.stage_at(StageId::Detect, r.reporter, None, flow, run);
                out.stage_at(StageId::ReportSent, r.reporter, None, flow, run);
                out.send(
                    n.lop,
                    ScmsMsg::Report(r),
                    "report",
                    self.sizes.report(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::Report(r) if to == n.lop => {
                out.stage_at(StageId::ProxyForwarded, to, None, flow, run);
                out.send(
                    n.ra,
                    ScmsMsg::Report(r),
                    "report-proxied",
                    self.sizes.report(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::Report(r) if to == n.ra => {
                self.ra.reports.push((run, *r));
                if !self.ra.report_shuffle_armed {
                    self.ra.report_shuffle_armed = true;
                    out.start_timer(
                        self.params.report_shuffle_window,
                        ScmsMsg::ReportShuffleTimer,
                        flow,
                        run,
                    );
                }
            }
            ScmsMsg::Report(r) => {
                Self::verify(out, 1);
                Self::scalar_mults(out, 1);
                self.ma.reports.push(*r);
                out.stage_at(StageId::ReportReceived, to, None, flow, run);
            }
            ScmsMsg::ReportShuffleTimer => {
                self.ra.report_shuffle_armed = false;
                for (r_run, report) in core::mem::take(&mut self.ra.reports) {
                    out.stage_at(StageId::Shuffled, to, None, FlowId::Report, r_run);
                    out.send(
                        n.ma,
                        ScmsMsg::Report(Box::new(report)),
                        "report-forward",
                        self.sizes.report(),
                        Transport::BackendNet,
                        FlowId::Report,
                        r_run,
                    );
                }
            }

            // ---------------- investigation and revocation ----------------
            ScmsMsg::PcaLookupRequest { i, lv } if to == n.ma => {
                out.stage_at(StageId::Decision, to, None, flow, run);
                out.send(
                    n.pca,
                    ScmsMsg::PcaLookupRequest { i, lv },
                    "pca-lookup-request",
                    self.sizes.pca_lookup_request(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::PcaLookupRequest { i, lv } => {
                let found = self.pca.issued.get(&(i, *lv.as_bytes())).copied();
                out.send(
                    n.ma,
                    ScmsMsg::PcaLookupResponse { found },
                    "pca-lookup-response",
                    self.sizes.pca_lookup_response(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::PcaLookupResponse { found } => {
                let Some(case) = self.ma.case.as_mut() else {
                    return Ok(());
                };
                let Some(found) = found else {
                    return Ok(());
                };
                case.lookups.push(found);
                if case.lookups.len() == 1 {
                    let (i, lv) = case.subjects[1];
                    out.send(
                        n.pca,
                        ScmsMsg::PcaLookupRequest { i, lv },
                        "pca-lookup-request",
                        self.sizes.pca_lookup_request(),
                        Transport::BackendNet,
                        flow,
                        run,
                    );
                } else if case.lookups.len() == 2 {
                    let (a, b) = (case.lookups[0], case.lookups[1]);
                    for (la, x, y) in [
                        (LaIndex::One, a.lci1, b.lci1),
                        (LaIndex::Two, a.lci2, b.lci2),
                    ] {
                        out.send(
                            self.nodes.la(la),
                            ScmsMsg::SameDeviceRequest { la, a: x, b: y },
                            "same-device-request",
                            self.sizes.same_device_request(),
                            Transport::BackendNet,
                            flow,
                            run,
                        );
                    }
                }
            }
            ScmsMsg::SameDeviceRequest { la, a, b } => {
                // The LA answers a single bit: whether the two chains belong to one
                // device. It never reveals which device, and the MA never learns a seed
                // from this exchange ([BRECHT §VI-C]).
                let st = &self.la[la.idx()];
                let same = match (st.owner.get(&a), st.owner.get(&b)) {
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                };
                out.send(
                    n.ma,
                    ScmsMsg::SameDeviceResponse { la, same },
                    "same-device-response",
                    self.sizes.same_device_response(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::SameDeviceResponse { la, same } => {
                let (ready, hash) = {
                    let Some(case) = self.ma.case.as_mut() else {
                        return Ok(());
                    };
                    case.same[la.idx()] = Some(same);
                    match (case.same[0], case.same[1]) {
                        (Some(a), Some(b)) => {
                            case.resolved = a && b;
                            (case.resolved, case.lookups[0].request_hash)
                        }
                        _ => (false, [0u8; 32]),
                    }
                };
                if ready {
                    self.ma.decisions.push(hash);
                    out.stage_at(StageId::Resolved, to, None, flow, run);
                    out.send(
                        n.ra,
                        ScmsMsg::BlocklistRequest { request_hash: hash },
                        "blocklist-request",
                        self.sizes.blocklist_request(),
                        Transport::BackendNet,
                        flow,
                        run,
                    );
                }
            }
            ScmsMsg::BlocklistRequest { request_hash } => {
                let rec = self.ra.requests.get(&request_hash).copied();
                if let Some(r) = rec {
                    self.ra.blocklist.insert(r.device);
                }
                out.stage_at(StageId::Blocklisted, to, None, flow, run);
                out.send(
                    n.ma,
                    ScmsMsg::BlocklistResponse {
                        known: rec.is_some(),
                        lci1: rec.and_then(|r| r.lci1).unwrap_or(Lci(0)),
                        lci2: rec.and_then(|r| r.lci2).unwrap_or(Lci(0)),
                    },
                    "blocklist-response",
                    self.sizes.blocklist_response(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::BlocklistResponse { known, lci1, lci2 } => {
                if !known {
                    return Ok(());
                }
                let Some(case) = self.ma.case.as_ref() else {
                    return Ok(());
                };
                let i_rev = case.i_rev;
                for (la, lci) in [(LaIndex::One, lci1), (LaIndex::Two, lci2)] {
                    out.send(
                        self.nodes.la(la),
                        ScmsMsg::SeedRequest { la, lci, i: i_rev },
                        "seed-request",
                        self.sizes.seed_request(),
                        Transport::BackendNet,
                        flow,
                        run,
                    );
                }
            }
            ScmsMsg::SeedRequest { la, lci, i } => {
                let st = &self.la[la.idx()];
                let Some(ls0) = st.chains.get(&lci).copied() else {
                    return Ok(());
                };
                // The LA releases `ls_x(i)` and not `ls_x(0)`. That single choice is what
                // makes revocation forward-only: nobody downstream can walk the chain
                // backwards, so the device's certificates from before period `i` stay
                // unlinkable ([BRECHT §VI-D]).
                Self::hashes(out, i);
                let seed = linkage::linkage_seed_at(st.la_id, ls0, i);
                let la_id = st.la_id;
                out.send(
                    n.ma,
                    ScmsMsg::SeedResponse { la, la_id, seed, i },
                    "seed-response",
                    self.sizes.seed_response(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::SeedResponse { la, la_id, seed, i } => {
                let (ready, jmax, crl_run) = {
                    let Some(case) = self.ma.case.as_mut() else {
                        return Ok(());
                    };
                    case.seeds[la.idx()] = Some((la_id, seed));
                    (
                        case.seeds[0].is_some() && case.seeds[1].is_some(),
                        case.jmax,
                        case.crl_run,
                    )
                };
                if ready {
                    let case = self.ma.case.as_ref().expect("checked");
                    let (la1, la2) = (
                        case.seeds[0].expect("both present"),
                        case.seeds[1].expect("both present"),
                    );
                    out.send(
                        n.crlg,
                        ScmsMsg::CrlAppend { i, la1, la2, jmax },
                        "crl-append",
                        self.sizes.crl_append(),
                        Transport::BackendNet,
                        FlowId::CrlIssuance,
                        crl_run,
                    );
                }
            }
            ScmsMsg::CrlAppend { i, la1, la2, jmax } => {
                let entry = CrlLinkageEntry {
                    i,
                    la_id1: la1.0,
                    la_id2: la2.0,
                    ls1_i: la1.1,
                    ls2_i: la2.1,
                    jmax,
                    max_forward: linkage::DEFAULT_MAX_FORWARD_PERIODS,
                };
                self.crlg.entries.push(entry);
                Self::sign(out, 1);
                let entries = u32::try_from(self.crlg.entries.len()).unwrap_or(u32::MAX);
                out.stage_at(
                    StageId::Issued,
                    to,
                    Some(self.sizes.crl(entries).bytes()),
                    flow,
                    run,
                );
                // The store first, then the broadcast path, so that two links with the
                // same parameters stamp `published` before `first_rsu_broadcast`.
                out.send(
                    n.crl_store,
                    ScmsMsg::CrlPublish { entries },
                    "crl-publish",
                    self.sizes.crl(entries),
                    Transport::BackendNet,
                    flow,
                    run,
                );
                out.send(
                    n.crl_broadcast,
                    ScmsMsg::CrlPublish { entries },
                    "crl-broadcast",
                    self.sizes.crl(entries),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            ScmsMsg::CrlPublish { entries } => {
                let list = self.crlg.entries.clone();
                let size = self.sizes.crl(entries).bytes();
                if to == n.crl_store {
                    self.crl_store.entries = list;
                    out.stage_at(StageId::Published, to, Some(size), flow, run);
                } else {
                    self.crl_broadcast.entries = list;
                    out.stage_at(StageId::FirstRsuBroadcast, to, Some(size), flow, run);
                }
            }

            // ---------------- CRL distribution ----------------
            ScmsMsg::CrlDownloadRequest { device } if to == device => {
                out.send(
                    n.crl_store,
                    ScmsMsg::CrlDownloadRequest { device },
                    "crl-request",
                    self.sizes.crl_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::CrlDownloadRequest { device } => {
                let entries = u32::try_from(self.crl_store.entries.len()).unwrap_or(u32::MAX);
                out.send(
                    device,
                    ScmsMsg::CrlDownload { device, entries },
                    "crl-download",
                    self.sizes.crl(entries),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            ScmsMsg::CrlAirBroadcast { device } => {
                // The roadside unit puts the list it holds on the air. It performs no
                // cryptography: the CRL Generator already signed it, and re-signing at
                // every RSU would be both wrong and a cost the deployment does not pay.
                let entries = u32::try_from(self.crl_broadcast.entries.len()).unwrap_or(u32::MAX);
                out.send(
                    device,
                    ScmsMsg::CrlDownload { device, entries },
                    "crl-air-broadcast",
                    self.sizes.crl(entries),
                    Transport::V2xAir,
                    flow,
                    run,
                );
            }
            ScmsMsg::CrlDownload { device, entries } => {
                let size = self.sizes.crl(entries).bytes();
                out.stage_at(StageId::Downloaded, device, Some(size), flow, run);
                // One signature verification over the list, then per entry: two SHA-256
                // per i-period walked and two AES per index searched ([ACPC §2],
                // [BRECHT §VII]).
                Self::verify(out, 1);
                // The device processes the list it *received*, from whichever path
                // delivered it. The two paths carry the same signed artefact, and reading
                // the store's copy on the broadcast path would make an RSU-only vehicle
                // silently enforce a CRL it never heard.
                let list = if from == n.crl_broadcast {
                    self.crl_broadcast.entries.clone()
                } else {
                    self.crl_store.entries.clone()
                };
                let Some(dev) = self.devices.get_mut(&device) else {
                    return Err(ProtoError::NoEntity { node: device });
                };
                let periods: u32 = dev
                    .credentials
                    .keys()
                    .map(|&(i, _)| i)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                let mut hashes = 0u32;
                let mut aes = 0u32;
                for e in &list {
                    let walked = periods.saturating_sub(e.i).min(e.max_forward);
                    hashes = hashes.saturating_add(2u32.saturating_mul(walked));
                    aes = aes
                        .saturating_add(2u32.saturating_mul(e.jmax).saturating_mul(walked.max(1)));
                }
                dev.crl = list;
                let silenced = !dev.revoked_credentials().is_empty();
                dev.silenced = silenced;
                Self::hashes(out, hashes);
                Self::aes(out, aes);
                out.stage_at(StageId::Processed, device, None, flow, run);
                out.stage_at(StageId::Enforced, device, None, flow, run);
                let _ = at;
            }
        }
        Ok(())
    }
}

/// The hash the RA knows a provisioning request by.
///
/// Over the request's public material, so two different requests cannot collide and the
/// same request replayed is recognised — the "one request per period" check of
/// [CAMP-EE §2.2.7.6].
fn request_hash(req: &ProvisioningRequest) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(&req.device.index().to_be_bytes());
    if let Some(a) = req.signing_public.compressed() {
        bytes.extend_from_slice(&a);
    }
    if let Some(p) = req.encryption_public.compressed() {
        bytes.extend_from_slice(&p);
    }
    bytes.extend_from_slice(&req.ck);
    bytes.extend_from_slice(&req.ek);
    bytes.extend_from_slice(&req.start_i.to_be_bytes());
    bytes.extend_from_slice(&req.periods.to_be_bytes());
    bytes.extend_from_slice(&req.jmax.to_be_bytes());
    sha256(&bytes)
}

/// A device's own view, for an inspector.
pub fn device_summary(dev: &DeviceState) -> BTreeMap<&'static str, u64> {
    let mut m = BTreeMap::new();
    m.insert("credentials", dev.credentials.len() as u64);
    m.insert("crl_entries", dev.crl.len() as u64);
    m.insert("revoked", dev.revoked_credentials().len() as u64);
    m.insert("silenced", u64::from(dev.silenced));
    m
}

/// Time helper: the deployment's idea of "later", for tests that step the clock.
pub const fn after(t: SimTime, d: Duration) -> SimTime {
    d.after(t)
}

/// The linkage value a device's credential `(i, j)` carries, for a test that needs to
/// report on it.
pub fn credential_lv(dev: &DeviceState, i: u32, j: u32) -> Option<LinkageValue> {
    dev.credentials.get(&(i, j)).map(|c| c.lv)
}

/// The size, in bytes, of a CRL with `entries` entries under this deployment's size model.
pub fn crl_bytes(sizes: &ScmsSizes, entries: u32) -> WireSize {
    sizes.crl(entries)
}
