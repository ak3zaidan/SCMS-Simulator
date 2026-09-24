//! The Phase 2 slice: an RSU with backhaul, the SCMS backend, an attacker, a detector, a
//! misbehaviour report and a revocation that reaches the other vehicle.
//!
//! 10-roadmap.md's Phase 2 is a *path*, not a feature list: a second vehicle detects a
//! first one lying about its position, files a misbehaviour report, the report crosses a
//! backhaul to the Misbehaviour Authority, two Linkage Authorities resolve two pseudonyms
//! to one device, the CRL Generator issues, the roadside broadcasts, and the vehicle that
//! filed the report stops trusting the liar. This module is the wiring of that path across
//! four crates, and it **reimplements none of them**:
//!
//! | Piece | Whose | What this module does with it |
//! |---|---|---|
//! | the attacker | [`v2xw_threat::LegacyAttacker`] | calls `act` on the outgoing claim before it is signed |
//! | the detector suite | [`v2xw_threat::Legacy12`] | feeds it the messages the node's own runtime delivered |
//! | the report | [`v2xw_threat::MisbehaviourReport`] | builds one `from_verdict` and puts it on the air |
//! | the backend | [`v2xw_proto::ScmsRun`] | provisions, submits, investigates, issues, broadcasts |
//! | the CRL | [`v2xw_sec::linkage::CrlLinkageEntry`] | installs it in the receiving node's own `CrlGate` |
//! | enforcement | [`v2xw_node::stores::CrlGate`] | the node's own revocation check does the rest |
//!
//! # The three joints, stated
//!
//! Every wiring of separately specified crates has joints, and the useful thing to do with
//! one is to say where it is.
//!
//! **1. The air credential is not the SCMS credential.** 03-interfaces.md §7 puts
//! enrolment and top-up in a `CredentialProtocol` plug-in and `v2xw-node` ships none, so a
//! node's *digest* on the air is still the `pseudo_signer` stand-in
//! ([`crate::wiring::bootstrap_credentials`]). What this module adds is the half that
//! revocation actually turns on: each credential carries the **linkage value** the SCMS
//! provisioning issued, and the i-period it belongs to. A CRL entry revokes linkage values
//! at a period, so [`v2xw_node::stores::CrlGate::check`] — which the node runtime already
//! calls on every verified frame — answers correctly without the digest being real.
//!
//! **2. The subject lookup is the engine's, standing in for the PCA's.** A report names a
//! certificate; the Misbehaviour Authority asks the PCA which `(i, linkage value)` that
//! certificate carries. Here the reporter names the subject by the digest it heard and
//! this module maps digest → `(i, lv)` from what it provisioned. That map is the PCA's job
//! and the mapping is one lookup either way; what it skips is the PCA's *service time*,
//! which the backend charges for the lookups the investigation itself makes.
//!
//! **3. The backend keeps its own clock.** [`v2xw_proto::ScmsRun`] is a discrete-event
//! deployment with its own kernel, and running it to quiescence is how its stage
//! timestamps get produced. So the engine does not interleave the two clocks: it runs the
//! backend to quiescence, reads the **latency** the stage log decomposes, and applies that
//! latency on its own timeline by scheduling the roadside broadcast for
//! `now + total_latency`. The decomposition 05-protocols §8 asks for is the backend's and
//! is exact; what the engine timeline sees is its sum.
//!
//! # What is not here
//!
//! The RSU is an [`v2xw_node::ObuRuntime`] with an RSU hardware profile and no message
//! services: it receives, its backhaul costs a stated latency, and the two application
//! messages this path needs (the report and the CRL broadcast) are put on the air by this
//! module rather than by an application layer inside the node.
//!
//! **That is now owed work and no longer a missing model.** 06-node-models.md §3's
//! roadside runtime — roles, failure states, a store-and-forward queue, a backhaul with
//! its own model card — ships in `v2xw-node` as [`v2xw_node::RsuRuntime`], with
//! `RsuRuntime::install_crl` as the custody this module fakes and `RsuStepOutcome` as the
//! transmissions and forwards it would return. What has not happened is moving the engine
//! onto it: [`crate::run::Engine`] holds one `BTreeMap<NodeId, ObuRuntime>` and steps it
//! in one parallel phase, and a second runtime type with a different `step` signature and
//! a different outcome is a change to that phase and to every `self.nodes.get` on this
//! path. Until then the role decisions here are this module's
//! ([`Phase2::rsu_has_role`]) rather than [`v2xw_node::RsuRoles`]'s, and the two agree on
//! the spellings on purpose — `RsuRole::as_str` produces `"crl"` and `"report-forward"`
//! precisely so that a scenario written against this wiring keeps working across the move.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::geom::Vec3;
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_proto::net::Transport;
use v2xw_proto::scms::run::{ScmsRun, crl_bytes};
use v2xw_proto::stage::{FlowRun, StageId};
use v2xw_proto::{RevocationLatency, ScmsParams};
use v2xw_sec::linkage::{CrlLinkageEntry, LinkageValue};
use v2xw_threat::{
    AttackKind, Attacker, AttackerView, Emission, Evidence, HonestClaim, Legacy12, LegacyAttacker,
    LegacyAttackerParams, MisbehaviourReport, NoMap, ObservedKind, ObservedMessage, SelfBelief,
    StationType, ThreatCtx, VerificationState,
};
use v2xw_world::World;

use crate::error::{EngineError, Result};
use crate::scenario::Scenario;

/// The protocol id a scenario names in `actors.backend.protocol` to get this path.
pub const CAMP_SCMS: &str = v2xw_proto::CAMP_SCMS_ID;

/// The detector suite's id, as `detection.local` names it.
pub const LEGACY_12: &str = "detect/legacy-12";

/// How the SCMS's device numbering is kept clear of the engine's.
///
/// [`v2xw_proto::ScmsNodes`] puts the eleven backend roles at 1..=11 in its own id space,
/// and the engine numbers its nodes from zero. The two spaces never meet — nothing passes
/// an engine [`NodeId`] to the backend or the other way — but a reader of a log should not
/// have to take that on trust, so a device is `SCMS_DEVICE_BASE + node.index()` and no
/// backend role id can be mistaken for a vehicle.
pub const SCMS_DEVICE_BASE: u32 = 1_000_000;

/// The mast height a roadside unit's antenna stands at above its ground position, metres.
///
/// The procedural world generator's own figure for the sites it makes, which is the
/// C2C-CC and USDOT pole-mounted deployment height; it is named here so a unit placed by
/// `position_m` and one placed at a `site` put their antennas at the same height.
pub const RSU_MAST_HEIGHT_M: f64 = 6.0;

/// The SCMS device id of an engine node.
fn device_of(node: NodeId) -> NodeId {
    NodeId::new(SCMS_DEVICE_BASE + node.index())
}

/// One credential the SCMS provisioning issued, as the engine installs it on the air.
#[derive(Debug, Clone, Copy)]
pub struct ProvisionedCred {
    /// The i-period.
    pub i: u32,
    /// The index within the period.
    pub j: u32,
    /// The linkage value the certificate's `linkageData` carries, and the thing a CRL
    /// entry revokes.
    pub lv: LinkageValue,
    /// The start of the certificate's validity window.
    pub valid_from: SimTime,
    /// Its end.
    pub valid_until: SimTime,
}

/// One roadside unit the scenario declared.
#[derive(Debug, Clone)]
pub struct RsuSpec {
    /// Where it stands, world-local metres — the site's antenna phase centre.
    pub position: Vec3,
    /// What it does: `crl`, `report-forward`, …
    pub roles: Vec<String>,
    /// Its hardware profile id.
    pub profile: String,
    /// One-way backhaul latency to the backend.
    pub backhaul: Duration,
}

impl RsuSpec {
    /// Whether this unit carries a role.
    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// An armed attacker: the model, and the window it is allowed to act in.
struct AttackerSlot {
    model: Box<dyn Attacker>,
    id: String,
    from: SimTime,
    to: SimTime,
}

/// What the scenario asked for, before any node exists to be it.
#[derive(Debug, Clone)]
struct AttackerSpec {
    id: String,
    params: LegacyAttackerParams,
    count: Option<u32>,
    fraction: Option<f64>,
    actor_ids: BTreeSet<u32>,
    from: SimTime,
    to: SimTime,
}

/// A report waiting at, or held by, the Misbehaviour Authority.
#[derive(Debug, Clone)]
pub struct HeldReport {
    /// The report itself.
    pub report: MisbehaviourReport,
    /// The i-period of the subject's certificate, as the engine's stand-in for the PCA
    /// lookup resolved it.
    pub subject_i: u32,
    /// The subject's linkage value.
    pub subject_lv: LinkageValue,
    /// The backend flow run **this** report's submission stamped its stages under.
    ///
    /// Per report and not one for the whole path: the decomposition 05-protocols §8 asks
    /// for starts at a `detect` stamp, and which report's `detect` that is decides the
    /// number. See [`Phase2::on_report_received`].
    pub run: FlowRun,
}

/// The revocation, once the backend has issued and broadcast it.
#[derive(Debug, Clone)]
pub struct Revocation {
    /// The CRL entry the CRL Generator issued.
    pub entry: CrlLinkageEntry,
    /// The decomposition 05-protocols §8 asks for: every stage that was reached, in order.
    pub stages: Vec<(StageId, SimTime)>,
    /// The whole backend latency, from the detector firing to the device enforcing.
    pub latency: Duration,
    /// The CRL's size on the air, bytes.
    pub bytes: u32,
    /// Which engine node was revoked.
    pub subject: NodeId,
}

/// Counters the run report carries for this path.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Phase2Report {
    /// How many roadside units were created.
    pub rsus: u64,
    /// Received envelopes that would not parse at all.
    pub spdu_parse_failures: u64,
    /// Received envelopes that parsed but whose signature did not verify.
    pub spdu_signature_failures: u64,
    /// How many received messages landed in each node verification state.
    ///
    /// Counted because the detector suite's behaviour is dominated by this one input, and
    /// the aggregate verdict count cannot tell "the signature failed" from "this node
    /// never checked" or "this node holds no certificate for the signer". Those are three
    /// different facts and only one of them is misbehaviour.
    pub verification_states: std::collections::BTreeMap<String, u64>,
    /// How many times each detector fired, by detector id.
    ///
    /// Reported because the aggregate `verdicts_fired` cannot distinguish a suite in which
    /// every check contributes a little from one in which a single mis-calibrated check
    /// accounts for nearly all of it. Those need opposite responses, and the aggregate
    /// alone sent one investigation down the wrong path.
    pub verdicts_by_detector: std::collections::BTreeMap<String, u64>,
    /// How many nodes were armed as attackers.
    pub attackers: u64,
    /// How many outgoing claims an attacker falsified.
    pub falsified_claims: u64,
    /// How many messages the local detectors checked.
    pub messages_checked: u64,
    /// How many detector verdicts fired.
    pub verdicts_fired: u64,
    /// How many misbehaviour reports went on the air.
    pub reports_sent: u64,
    /// How many reached the Misbehaviour Authority over a backhaul.
    pub reports_received: u64,
    /// How many candidate pairs the authority opened a linkage resolution over.
    pub cases_opened: u64,
    /// How many of those the two Linkage Authorities said were different devices, so no
    /// entry was issued. The cost of a false report, in backend lookups.
    pub cases_unresolved: u64,
    /// How many CRL entries the backend issued.
    pub crls_issued: u64,
    /// How many CRL broadcasts the roadside put on the air.
    pub crl_broadcasts: u64,
    /// How many issued revocations could not be broadcast because the backend's own
    /// latency put the broadcast past the run horizon.
    ///
    /// Counted rather than silent, because it is the one way this path can produce a
    /// revocation that never reaches a vehicle for a reason that is not a modelling
    /// choice: a run too short for the deployment's reporting latency. A reader seeing
    /// `crls_issued` without `crl_broadcasts` needs this number to tell "the roadside is
    /// not wired up" from "the scenario ends before the CRL would have been issued".
    pub crl_past_horizon: u64,
    /// How many nodes installed the CRL entry.
    pub crls_installed: u64,
    /// How many receptions the installed CRL caused to be classified revoked.
    pub revoked_receptions: u64,
    /// The backend's revocation latency in nanoseconds, once there is one.
    pub revocation_latency_ns: u64,
}

/// The Phase 2 state of one run.
pub struct Phase2 {
    scms: ScmsRun,
    /// Certificates per i-period the provisioning asks for. Two is the minimum a linkage
    /// resolution needs: the authority correlates *two* pseudonyms, which is the whole
    /// reason it asks two Linkage Authorities whether they belong to one device.
    jmax: u32,
    /// The credentials provisioned for each engine node.
    creds: BTreeMap<NodeId, Vec<ProvisionedCred>>,
    /// The digest → `(node, i, j)` map that stands in for the PCA's certificate lookup.
    ///
    /// Keyed by the digest the certificate is **announced under on the air**, which is not
    /// the digest the credential was installed with: `ObuRuntime` issues a real
    /// certificate for every pseudonym on its first transmission and writes that
    /// certificate's own `HashedId8` back into the store, so a map built at spawn time
    /// holds the [`v2xw_node::stores::pseudo_signer`] stand-ins and resolves nothing. The
    /// engine therefore registers a digest when the node hands a frame down
    /// ([`Phase2::note_digest`]), which is the moment it is knowable and the only moment
    /// it can be reported.
    by_digest: BTreeMap<[u8; 8], (NodeId, u32, u32)>,
    rsus: Vec<RsuSpec>,
    /// The roadside units' node ids, once the engine has created them.
    rsu_nodes: Vec<NodeId>,
    specs: Vec<AttackerSpec>,
    attackers: BTreeMap<NodeId, AttackerSlot>,
    detectors: BTreeMap<NodeId, Legacy12>,
    detection_on: bool,
    /// How many vehicles have been offered to the selection rule, which is what `count`
    /// counts.
    vehicles: u64,
    /// Which (reporter, subject) pairs have already been filed, so one detector does not
    /// file the same subject every tenth of a second.
    filed: BTreeSet<(NodeId, String)>,
    held: Vec<HeldReport>,
    revocation: Option<Revocation>,
    report: Phase2Report,
}

impl core::fmt::Debug for Phase2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Phase2")
            .field("rsus", &self.rsus.len())
            .field("attackers", &self.attackers.len())
            .field("detectors", &self.detectors.len())
            .field("held_reports", &self.held.len())
            .field("revoked", &self.revocation.is_some())
            .finish_non_exhaustive()
    }
}

impl Phase2 {
    /// Builds the Phase 2 state a scenario declared, or `None` when it declared none.
    ///
    /// # Errors
    /// [`EngineError::Scenario`] when the scenario names a protocol, a detector, an
    /// attacker model or a world site this build does not have — by field name, so the
    /// message says what to change.
    pub fn build(scenario: &Scenario, world: &World) -> Result<Option<Phase2>> {
        let wants_backend = scenario.actors.backend.protocol.is_some();
        let wants_rsus = !scenario.actors.rsus.is_empty();
        let wants_threats = !scenario.threats.attackers.is_empty();
        let wants_detection = !scenario.detection.local.is_empty();
        if !(wants_backend || wants_rsus || wants_threats || wants_detection) {
            return Ok(None);
        }

        if let Some(protocol) = &scenario.actors.backend.protocol
            && protocol != CAMP_SCMS
        {
            return Err(conflict(
                "actors.backend.protocol",
                format!("this build ships one credential protocol, {CAMP_SCMS}; got {protocol}"),
            ));
        }
        for choice in &scenario.detection.local {
            if choice.id != LEGACY_12 {
                return Err(conflict(
                    "detection.local",
                    format!(
                        "this build ships one local detector suite, {LEGACY_12}; got {}",
                        choice.id
                    ),
                ));
            }
        }

        // The roadside units. `site` indexes the world's own infrastructure sites, which is
        // where an RSU may stand; refusing with the count is what tells the author whether
        // the world has a site table at all.
        let mut rsus = Vec::new();
        for spec in &scenario.actors.rsus {
            let position = match (spec.site, spec.position_m) {
                (Some(site), None) => {
                    let site = world.sites.get(site as usize).ok_or_else(|| {
                        conflict(
                            "actors.rsus[].site",
                            format!(
                                "site {site} does not exist: this world has {} \
                                 infrastructure site(s). The OSM importer produces none — an \
                                 extract carries road geometry and buildings, not a mast \
                                 inventory — so a scenario on an imported city states \
                                 `position_m` instead; the procedural generator produces one \
                                 site per junction with `rsu_at_junctions: true`.",
                                world.sites.len()
                            ),
                        )
                    })?;
                    site.antenna_position()
                }
                (None, Some(p)) => {
                    // The mast, from the ground position the scenario gave and the height
                    // `v2xw_world::Site` would have carried. 6 m is the C2C-CC and USDOT
                    // deployment figure for a pole-mounted RSU and the value the procedural
                    // generator uses for its own sites, so a scenario that states a position
                    // and one that names a site put the antenna at the same height.
                    Vec3::new(p[0], p[1], p[2] + RSU_MAST_HEIGHT_M)
                }
                _ => {
                    return Err(conflict(
                        "actors.rsus[]",
                        "a roadside unit stands either at a world `site` or at an explicit \
                         `position_m`; give exactly one",
                    ));
                }
            };
            rsus.push(RsuSpec {
                position,
                roles: spec.roles.clone(),
                profile: spec
                    .profile
                    .clone()
                    // `v2xw-node` names no reference RSU, so the default is stated here:
                    // the Cohda MK5 RSU, whose profile ships with the crate and whose CPU
                    // and HSM rates come from the same published figures the OBU's do.
                    .unwrap_or_else(|| "rsu/cohda-mk5-rsu".to_string()),
                // 06-node-models.md §3 has the backhaul as its own link model and this
                // build ships none, so the latency is the SCMS deployment's own figure for
                // a backend link rather than a number invented here.
                backhaul: ScmsParams::default().backend_link_latency,
            });
        }

        // The attackers. `LegacyAttacker` is the ported legacy family; the id a scenario
        // writes is `threat/attacker/legacy/<AttackKind>`.
        let mut specs = Vec::new();
        // An `attack.wave` on the timeline is the schedule of every population it names
        // (`crate::timeline::attack_windows`): the population acts inside the wave and not
        // outside it. The threat crate's `AttackSchedule` carries one window, which is why
        // `validate` refuses a population named by two waves.
        let waves = crate::timeline::attack_windows(scenario);
        for (population, a) in scenario.threats.attackers.iter().enumerate() {
            let kind =
                a.id.strip_prefix("threat/attacker/legacy/")
                    .and_then(AttackKind::parse)
                    .ok_or_else(|| {
                        conflict(
                            "threats.attackers[].id",
                            format!(
                                "{} is not an attacker this build ships: the legacy family is \
                             `threat/attacker/legacy/<Kind>`, e.g. \
                             threat/attacker/legacy/ConstPos",
                                a.id
                            ),
                        )
                    })?;
            let mut params = LegacyAttackerParams::new(kind);
            if !a.params.is_null() {
                if let Some(intensity) = a.params.get("intensity").and_then(|v| v.as_f64()) {
                    params.intensity = intensity;
                }
                if let Some(dt) = a.params.get("dt_s").and_then(|v| v.as_f64()) {
                    params.dt_s = dt;
                }
            }
            let horizon = (scenario.time.duration_s * 1e9).round().max(0.0) as u64;
            let (from, to) = match (waves.get(&population), &a.schedule) {
                (Some((from_s, to_s)), _) => (
                    (from_s * 1e9).round().max(0.0) as u64,
                    (to_s * 1e9).round().max(0.0) as u64,
                ),
                (None, Some(w)) => (
                    (w.from_s * 1e9).round().max(0.0) as u64,
                    (w.to_s * 1e9).round().max(0.0) as u64,
                ),
                (None, None) => (0, horizon),
            };
            specs.push(AttackerSpec {
                id: a.id.clone(),
                params,
                count: a.count,
                fraction: a.fraction,
                actor_ids: a.actor_ids.iter().copied().collect(),
                from,
                to,
            });
        }

        // The backend. `quick()` shortens the two batching windows and leaves every cited
        // number alone, which is what makes a sixty-second scenario able to reach a
        // revocation at all: the CAMP shuffle window is "10,000 requests or one day".
        let jmax = 2;
        let scms = ScmsRun::new(ScmsParams::default().quick()).map_err(|e| {
            conflict(
                "actors.backend",
                format!("the SCMS deployment refused to start: {e}"),
            )
        })?;
        Ok(Some(Phase2 {
            scms,
            jmax,
            creds: BTreeMap::new(),
            by_digest: BTreeMap::new(),
            rsus,
            rsu_nodes: Vec::new(),
            specs,
            attackers: BTreeMap::new(),
            detectors: BTreeMap::new(),
            detection_on: wants_detection,
            vehicles: 0,
            filed: BTreeSet::new(),
            held: Vec::new(),
            revocation: None,
            report: Phase2Report::default(),
        }))
    }

    /// The roadside units to create, in declaration order.
    pub fn rsu_specs(&self) -> &[RsuSpec] {
        &self.rsus
    }

    /// Records the node id the engine gave one roadside unit.
    pub fn note_rsu(&mut self, node: NodeId) {
        self.rsu_nodes.push(node);
        self.report.rsus += 1;
    }

    /// The roadside units' node ids.
    pub fn rsu_nodes(&self) -> &[NodeId] {
        &self.rsu_nodes
    }

    /// The unit `node` is, if it is one.
    ///
    /// `rsu_nodes` is filled by [`Phase2::note_rsu`] in the order
    /// [`Phase2::rsu_specs`] yields, because [`crate::run::Engine`]'s `create_rsus` walks
    /// the specs once and creates one node per spec, so index `k` of the one is index `k`
    /// of the other. Pairing them here is what makes a role a property of a *unit*: the two
    /// role decisions on this path used to ask whether **any** declared unit carried the
    /// role, so with two masts the one without it broadcast too, and the counterexample
    /// test that says a unit with no `crl` role broadcasts nothing only held because the
    /// scenario declares exactly one.
    #[must_use]
    pub fn rsu_spec_of(&self, node: NodeId) -> Option<&RsuSpec> {
        let index = self.rsu_nodes.iter().position(|n| *n == node)?;
        self.rsus.get(index)
    }

    /// Whether `node` is a roadside unit that carries `role`.
    ///
    /// A unit that declares no role at all carries every one of them: the scenario schema
    /// defaults `roles` to empty, and a unit that did nothing would be a unit an author
    /// has to remember to configure before anything works.
    #[must_use]
    pub fn rsu_has_role(&self, node: NodeId, role: &str) -> bool {
        self.rsu_spec_of(node)
            .is_some_and(|s| s.has_role(role) || s.roles.is_empty())
    }

    /// The roadside units that carry `role`, in declaration order.
    #[must_use]
    pub fn rsus_with_role(&self, role: &str) -> Vec<NodeId> {
        self.rsu_nodes
            .iter()
            .copied()
            .filter(|n| self.rsu_has_role(*n, role))
            .collect()
    }

    /// The counters for the run report.
    /// The counters, mutably, for the engine to fold node-local totals into.
    pub fn report_mut(&mut self) -> &mut Phase2Report {
        &mut self.report
    }

    /// The counters this path accumulated during the run.
    pub fn report(&self) -> &Phase2Report {
        &self.report
    }

    /// The revocation, once there is one.
    pub fn revocation(&self) -> Option<&Revocation> {
        self.revocation.as_ref()
    }

    /// Enrols and provisions one node with the backend, returning the credentials to
    /// install on the air.
    ///
    /// The backend runs to quiescence here, on its **own** clock, which is what "the
    /// vehicle was provisioned before the run" means: enrolment and the first certificate
    /// batch are a dealership-time flow measured by 05-protocols §3, not something that
    /// happens while the vehicle is driving. A scenario measuring provisioning latency
    /// reads the flow's stage stamps; a scenario measuring revocation, as this one does,
    /// needs the certificates to exist before the first frame.
    pub fn provision(&mut self, node: NodeId) -> Vec<ProvisionedCred> {
        let device = device_of(node);
        self.scms.add_device(device);
        self.scms.enrol(device);
        if self.scms.run().is_err() {
            return Vec::new();
        }
        self.scms.provision(device, 0, 1, self.jmax);
        if self.scms.run().is_err() {
            return Vec::new();
        }
        let params = ScmsParams::default().quick();
        let Some(dev) = self.scms.state.devices.get(&device) else {
            return Vec::new();
        };
        let mut out: Vec<ProvisionedCred> = Vec::new();
        for j in 0..self.jmax {
            if let Some(cred) = dev.credentials.get(&(0, j)) {
                let (from, until) = params.validity(0);
                out.push(ProvisionedCred {
                    i: 0,
                    j,
                    lv: cred.lv,
                    valid_from: from,
                    valid_until: until,
                });
            }
        }
        self.creds.insert(node, out.clone());
        out
    }

    /// Registers the air digest one provisioned credential is carried under.
    ///
    /// This is joint 2: the map a report's subject digest is resolved through, standing in
    /// for the Misbehaviour Authority asking the PCA about a certificate. `i` and `j` name
    /// the pseudonym, so the lookup answers with **that pseudonym's** linkage value and
    /// not merely with the device — two reports about two pseudonyms are then two
    /// different linkage values, which is what the Linkage Authorities are asked to
    /// correlate.
    ///
    /// Called from the transmit path rather than once at spawn, because a credential's
    /// digest is not fixed at spawn: see [`Phase2::by_digest`]. It is idempotent, and a
    /// pseudonym the node never transmits under is a pseudonym nothing can report.
    pub fn note_digest(
        &mut self,
        node: NodeId,
        digest: &v2xw_msg::sec_types::HashedId8,
        i: u32,
        j: u32,
    ) {
        self.by_digest.insert(digest_key(digest), (node, i, j));
    }

    /// Arms this node as an attacker if the scenario's selection rule names it.
    ///
    /// The three selection rules of 03-interfaces.md §13 and what each counts:
    ///
    /// * `actor_ids` names actors, which is the only rule that survives a change to the
    ///   demand rate;
    /// * `count` takes the first `count` **vehicles**, counted here rather than from the
    ///   engine's node counter — the roadside units are nodes and are created first, so a
    ///   count over nodes would spend the attacker budget on masts;
    /// * `fraction` draws per node from `(Attack, Node)`, a keyed stream, so whether a
    ///   vehicle is an attacker does not depend on how many spawned before it.
    pub fn arm_attacker(&mut self, ctx: &mut dyn ThreatCtx, node: NodeId, actor: ActorId) -> bool {
        let index = self.vehicles;
        self.vehicles += 1;
        for spec in &self.specs {
            let selected = if !spec.actor_ids.is_empty() {
                spec.actor_ids.contains(&actor.index())
            } else if let Some(count) = spec.count {
                index < u64::from(count)
            } else if let Some(fraction) = spec.fraction {
                ctx.rng(
                    v2xw_core::rng::RngDomain::Attack,
                    v2xw_core::rng::EntityRef::Node(node),
                )
                .bool(fraction)
            } else {
                false
            };
            if !selected {
                continue;
            }
            // The schedule is the attacker model's own, and the model gates on it: the
            // window a scenario writes is the window `AttackSchedule::active_at` enforces,
            // not a second gate here that could disagree with it.
            let schedule = v2xw_threat::AttackSchedule {
                from: spec.from,
                to: spec.to,
                ..v2xw_threat::AttackSchedule::default()
            };
            self.attackers.insert(
                node,
                AttackerSlot {
                    model: Box::new(LegacyAttacker::new(
                        node,
                        spec.params.clone(),
                        // The insider of 07-threats §2.2: valid credentials, a conforming
                        // radio and map knowledge. `jmax` pseudonyms, because that is what
                        // the SCMS provisioned for this device — an attacker cannot hold
                        // more credentials than the backend issued it.
                        v2xw_threat::Capabilities::insider(self.jmax),
                        schedule.clone(),
                        // No ghost signers: a Sybil attacker's extra identities are
                        // credentials the engine's store issues, and the credential
                        // protocol that would issue a second concurrent one does not ship.
                        Vec::new(),
                    )),
                    id: spec.id.clone(),
                    from: spec.from,
                    to: spec.to,
                },
            );
            self.report.attackers += 1;
            return true;
        }
        if self.detection_on {
            self.detectors.insert(node, Legacy12::legacy_defaults());
        }
        false
    }

    /// Whether this node is an armed attacker.
    #[must_use]
    pub fn is_attacker(&self, node: NodeId) -> bool {
        self.attackers.contains_key(&node)
    }

    /// Lets an attacker edit the claim that is about to be signed.
    ///
    /// Called immediately before the frame is built, which is where
    /// [`v2xw_threat::Attacker::act`] is specified to run: "an edit here is an edit to the
    /// bytes that go on the air, and everything after it — signing cost, MAC, DCC — is the
    /// ordinary node path".
    #[allow(clippy::too_many_arguments)]
    pub fn falsify(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        node: NodeId,
        actor: ActorId,
        believed_time: SimTime,
        signer: [u8; 8],
        honest: HonestClaim,
        belief: SelfBelief,
        cert: (SimTime, SimTime),
        msg: u64,
    ) -> Option<Emission> {
        let slot = self.attackers.get_mut(&node)?;
        if believed_time < slot.from || believed_time >= slot.to {
            return None;
        }
        let view = AttackerView {
            // The attacker sees what this node received, which the engine does not hand it
            // here: an attacker that reads its own receive history is a feedback attacker
            // and this build wires the open-loop families. The slice is empty rather than
            // ground truth, which is the honest degenerate value (invariant I-T1).
            own_rx: &[],
            own_credentials: &[],
            crl_revocations_seen: None,
            own_belief: belief,
            honest,
            believed_time,
        };
        let mut out = Emission::honest(signer, honest, believed_time, cert.0, cert.1);
        let actions = slot.model.act(ctx, &view, &mut out);
        if actions.is_empty() {
            return Some(out);
        }
        let id = slot.id.clone();
        v2xw_threat::log_actions(ctx, believed_time, actor, &id, &actions, Some(msg));
        if v2xw_threat::is_falsified(&honest, &out, believed_time, StationType::Vehicle) {
            self.report.falsified_claims += 1;
        }
        Some(out)
    }

    /// Runs the local detector suite over what one node's runtime delivered, returning any
    /// report the node decided to file.
    ///
    /// The detector sees `me` — the node's own belief — and the messages, and nothing else:
    /// [`v2xw_threat::Detector::on_message`] has no world argument, which is invariant I-T2
    /// expressed as a signature.
    pub fn detect(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        node: NodeId,
        me: &SelfBelief,
        delivered: &[v2xw_node::VerifiedMessage],
    ) -> Vec<MisbehaviourReport> {
        let Some(detector) = self.detectors.get_mut(&node) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for m in delivered {
            let Some(signer) = &m.signer else { continue };
            let key = digest_key(signer);
            let claimed = m.claimed_pos.unwrap_or(Vec3::ZERO);
            let observed = ObservedMessage {
                signer: key,
                kind: match m.msg_type {
                    // The cause code is what a DENM check reads, and the generator emits no
                    // DENM, so an empty cause is unreachable rather than a silent default.
                    v2xw_msg::MsgType::Denm => ObservedKind::Denm(String::new()),
                    _ => ObservedKind::Beacon,
                },
                received_at: m.received_at,
                claimed_generation_time: m.claimed_generation_time,
                claimed_x_m: claimed.x,
                claimed_y_m: claimed.y,
                claimed_speed_mps: m.claimed_speed_mps,
                claimed_heading_rad: m.claimed_heading_rad,
                // The generator emits no position-confidence field, so the value the
                // subject "broadcasts" is the legacy suite's own default rather than a
                // number read off a message that does not carry one.
                claimed_pos_confidence_m: 5.0,
                repetitions: 1,
                cert_valid_from: 0,
                cert_valid_to: SimTime::MAX,
                station_type: StationType::Vehicle,
                // The node's four states and the detector's four are not the same four:
                // `Revoked` is a *credential* conclusion and the detector suite's
                // vocabulary has no word for it, so it maps to `UnknownCertificate` — the
                // state that also zeroes every content check, which is the conservative
                // reading (a revoked certificate's claims are not evidence).
                verification: match m.verification {
                    v2xw_node::stores::VerificationState::Verified => VerificationState::Valid,
                    v2xw_node::stores::VerificationState::Invalid => {
                        VerificationState::BadSignature
                    }
                    v2xw_node::stores::VerificationState::Revoked => {
                        VerificationState::UnknownCertificate
                    }
                    _ => VerificationState::Unverified,
                },
            };
            let verdict = v2xw_threat::Detector::on_message(detector, ctx, me, &observed, &NoMap);
            self.report.messages_checked += 1;
            *self
                .report
                .verification_states
                .entry(format!("{:?}", m.verification))
                .or_insert(0) += 1;

            if !verdict.fired() {
                continue;
            }
            self.report.verdicts_fired += 1;
            for o in &verdict.fired {
                *self
                    .report
                    .verdicts_by_detector
                    .entry(o.detector.as_str().to_string())
                    .or_insert(0) += 1;
            }
            if !self.filed.insert((node, verdict.subject.clone())) {
                continue;
            }
            let evidence = Evidence::at(m.received_at, m.received_at, 5.0);
            let id = format!("r-{}-{}", node.index(), verdict.subject);
            if let Some(report) = MisbehaviourReport::from_verdict(
                id,
                node,
                v2xw_core::hash::hex_encode(&me.node.index().to_le_bytes()),
                &verdict,
                &evidence,
            ) {
                self.report.reports_sent += 1;
                out.push(report);
            }
        }
        out
    }

    /// Resolves a report's subject to the `(i, linkage value)` the backend needs.
    ///
    /// Joint 2 again, from the other end: this is the lookup the PCA performs.
    fn resolve_subject(&self, digest_hex: &str) -> Option<(NodeId, u32, LinkageValue)> {
        let bytes = decode_hex8(digest_hex)?;
        let (node, i, j) = *self.by_digest.get(&bytes)?;
        // The exact certificate the subject signed with, and therefore the linkage value
        // *that pseudonym* carries. Two pseudonyms of one device are two different linkage
        // values that resolve to the same device, which is the property the two Linkage
        // Authorities exist to provide and the thing the authority is asking them about.
        let cred = self
            .creds
            .get(&node)?
            .iter()
            .find(|c| c.i == i && c.j == j)?;
        Some((node, cred.i, cred.lv))
    }

    /// One report reaches the Misbehaviour Authority over a backhaul.
    ///
    /// Returns the revocation when this report completed a case. The authority needs
    /// **two** reports before it can ask the Linkage Authorities anything, and that is the
    /// whole reason the resolution is a two-party protocol: one report names one
    /// pseudonym, and one pseudonym is, by construction, not a device.
    ///
    /// # Why it tries pairs
    ///
    /// The authority cannot tell from two reports whether they are about one device — that
    /// is precisely what it is asking the Linkage Authorities. So it does what an authority
    /// does: it picks a candidate pair, spends the two lookups, and lets the answer decide.
    /// A pair that resolves to one device produces a CRL entry; a pair that does not is
    /// dropped and the next candidate is tried. Nothing here inspects who the subjects
    /// really are before asking, which is the property that makes a false report unable to
    /// revoke an innocent device: `case.same` comes back `[Some(false), _]` and no entry is
    /// issued. The run report's `verdicts_fired` against `crls_issued` is where the cost of
    /// those wasted pairs shows up.
    ///
    /// The candidate pairs are (every earlier report, this one), in arrival order, which is
    /// a deterministic walk over a `Vec` and not over anything hashed.
    pub fn on_report_received(
        &mut self,
        report: MisbehaviourReport,
        reporter: NodeId,
    ) -> Option<&Revocation> {
        self.report.reports_received += 1;
        let (subject, i, lv) = self.resolve_subject(&report.subject_cert_digest)?;
        let run = self.scms.submit_report(device_of(reporter), i, lv);
        self.held.push(HeldReport {
            report,
            subject_i: i,
            subject_lv: lv,
            run,
        });
        if self.scms.run().is_err() || self.held.len() < 2 || self.revocation.is_some() {
            return None;
        }
        let last = self.held.len() - 1;
        let i_rev = self.held[last].subject_i;
        for a in 0..last {
            let Some((resolution, issuance)) = self.scms.investigate(a, last, i_rev, self.jmax)
            else {
                continue;
            };
            self.report.cases_opened += 1;
            if self.scms.run().is_err() {
                continue;
            }
            let resolved = self.scms.state.ma.case.as_ref().is_some_and(|c| c.resolved);
            if !resolved {
                // The two Linkage Authorities said the pseudonyms belong to different
                // devices. No entry is issued, and the pair is not retried.
                self.report.cases_unresolved += 1;
                continue;
            }
            // The roadside broadcast path, which is the distribution 10-roadmap.md's
            // Phase 2 asks for. `attach_rsu` is not decoration: without a link the backend
            // kernel refuses to deliver rather than delivering for free (invariant I-P1).
            let victim = device_of(subject);
            self.scms.attach_rsu(victim);
            let distribution = self.scms.broadcast_crl_to(victim);
            if self.scms.run().is_err() {
                continue;
            }
            let entry = (*self.scms.crl_entry()?).clone();
            // The decomposition is over the report that **completed the case** — this
            // one — and not over the first report the authority ever received.
            //
            // This is a joint-3 consequence and it is the reason the distribution half of
            // the path never happened. The backend keeps its own clock and every call into
            // it here runs it to quiescence, so the stage log's instants are contiguous
            // only *within* one of those calls: the report shuffle window alone is a
            // minute of backend time per submission, and provisioning a vehicle that
            // spawned in between is two more. Measuring from the first report's `detect`
            // therefore charged the revocation for every submission and every provisioning
            // that happened before it — an interval that grows with the false-positive
            // rate and quickly exceeds the whole run. The engine then applied that
            // interval on its own timeline, the broadcast landed past the horizon, and
            // nothing was ever put on the air.
            //
            // This report's submission, the resolution it triggered, the issuance and the
            // distribution all happen inside this one call, so `detect` to `enforced` over
            // its run is a contiguous span of backend time and is the latency of *this*
            // revocation. The earlier report is the authority's prior evidence; it is what
            // made the case possible and it is not on the path being measured.
            let latency = RevocationLatency::assemble(
                &self.scms.kernel.stages,
                victim,
                Transport::V2xAir,
                self.held[last].run,
                resolution,
                issuance,
                distribution,
            );
            let stages = latency.stages.clone();
            let total = stages
                .last()
                .zip(stages.first())
                .map_or(Duration::ZERO, |(last, first)| {
                    Duration::from_nanos(last.1.saturating_sub(first.1))
                });
            let bytes = crl_bytes(&self.scms.state.sizes, 1).bytes();
            self.report.crls_issued += 1;
            self.report.revocation_latency_ns = total.as_nanos();
            self.revocation = Some(Revocation {
                entry,
                stages,
                latency: total,
                bytes,
                subject,
            });
            return self.revocation.as_ref();
        }
        None
    }

    /// Notes that the roadside put a CRL on the air.
    pub fn note_crl_broadcast(&mut self) {
        self.report.crl_broadcasts += 1;
    }

    /// Notes a revocation whose broadcast instant fell past the run horizon.
    pub fn note_crl_past_horizon(&mut self) {
        self.report.crl_past_horizon += 1;
    }

    /// Notes that a node installed the CRL entry.
    pub fn note_crl_installed(&mut self) {
        self.report.crls_installed += 1;
    }

    /// Notes a reception the installed CRL caused to be classified revoked.
    pub fn note_revoked_reception(&mut self) {
        self.report.revoked_receptions += 1;
    }

    /// The credentials provisioned for a node.
    pub fn creds(&self, node: NodeId) -> &[ProvisionedCred] {
        self.creds.get(&node).map_or(&[], Vec::as_slice)
    }
}

/// The eight bytes of a certificate digest, as this module keys its maps by.
fn digest_key(digest: &v2xw_msg::sec_types::HashedId8) -> [u8; 8] {
    let mut key = [0u8; 8];
    key.copy_from_slice(&digest.0[..]);
    key
}

/// Eight bytes from a sixteen-character hex string.
fn decode_hex8(s: &str) -> Option<[u8; 8]> {
    if s.len() < 16 {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn conflict(field: &str, message: impl Into<String>) -> EngineError {
    EngineError::Scenario(crate::ScenarioError::conflict(field, message.into()))
}

/// The eight bytes of a certificate digest, for a caller outside this module.
#[must_use]
pub fn digest_bytes(digest: &v2xw_msg::sec_types::HashedId8) -> [u8; 8] {
    digest_key(digest)
}

/// How many bytes a misbehaviour report takes on the air.
///
/// [`v2xw_proto`]'s own size model for a report submission, which carries its provenance:
/// 05-protocols marks the report's wire size as one of the five with no published value,
/// and the crate's `SizeParams` holds the figure with that provenance attached rather than
/// this module choosing a number.
#[must_use]
pub fn report_bytes() -> u32 {
    let sizes = ScmsParams::default().sizes;
    sizes.report_payload_bytes
}
