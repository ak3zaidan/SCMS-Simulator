//! The ETSI TS 102 941 enrolment and authorization flows.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::Duration;
use v2xw_sec::primitive::profiles;

use crate::error::Result;
use crate::kernel::{Delivery, Kernel, Outbox};
use crate::net::{BackendNet, Link, Transport};
use crate::service::ServiceModelSpec;
use crate::sizes::{
    AES128_KEY_BYTES, CertificateSizes, EC_POINT_COMPRESSED_BYTES, ECDSA_P256_SIG_COER_BYTES,
    ECIES_P256_ENCRYPTED_KEY_BYTES, ENVELOPE_OVERHEAD_CERT_BYTES, ENVELOPE_OVERHEAD_DIGEST_BYTES,
    HASHED_ID8_BYTES, SHA256_BYTES, WireSize,
};
use crate::spec::{
    Centrality, CredentialTypeSpec, EntityRoleSpec, FlowSpec, HolderKind, PassiveRevocation,
    ProtocolId, RevocationMechanism, SeparationRule, TrustBoundary, ValidityPolicy,
};
use crate::stage::{FlowId, FlowRun, StageId};

/// The plug-in's stable id.
pub const ETSI_TS102941_ID: &str = "protocol/etsi/ts102941";

const STRUCTURE: &str = "TS 102 941 V2.2.1 §6.2.3.2-§6.2.3.4 (structures, hand-written per build decision D5); \
     field sizes from 04-models.md §9.1, SEC 1 §2.3.3, FIPS 197 §5, FIPS 180-4, \
     Ieee1609Dot2BaseTypes.asn";

/// Which node hosts which ETSI role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EtsiNodes {
    /// Enrolment Authority.
    pub ea: NodeId,
    /// Authorization Authority.
    pub aa: NodeId,
    /// Root CA.
    pub rca: NodeId,
    /// Trust List Manager.
    pub tlm: NodeId,
    /// Central Point of Contact / Distribution Centre.
    pub cpoc: NodeId,
    /// Misbehaviour Authority.
    pub ma: NodeId,
}

impl Default for EtsiNodes {
    fn default() -> EtsiNodes {
        EtsiNodes {
            ea: NodeId::new(101),
            aa: NodeId::new(102),
            rca: NodeId::new(103),
            tlm: NodeId::new(104),
            cpoc: NodeId::new(105),
            ma: NodeId::new(106),
        }
    }
}

impl EtsiNodes {
    /// The links the two built flows need.
    pub fn links(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.aa, self.ea),
            (self.rca, self.ea),
            (self.rca, self.aa),
            (self.tlm, self.cpoc),
            (self.cpoc, self.ea),
            (self.cpoc, self.aa),
            (self.ma, self.ea),
            (self.ma, self.aa),
        ]
    }
}

/// The skeleton's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EtsiParams {
    /// Enrolment-credential validity: three years [EUCP Table 11].
    pub ec_validity: Duration,
    /// Authorization-ticket validity: one week [EUCP §7.2.1].
    pub at_validity: Duration,
    /// Concurrent authorization tickets: 100 in the EU Certificate Policy, 20 in the
    /// C2C-CC basic system profile [EUCP §7.2.1; TR 103 415 Table A.2].
    pub at_concurrent: u32,
    /// Preload horizon: three months [EUCP §7.2.1].
    pub at_preload: Duration,
    /// The subject-attribute container's size — not fixed by any clause.
    pub subject_attributes_bytes: u32,
    /// Backend servers per entity.
    pub servers: u32,
    /// Per-request overhead at a backend entity.
    pub overhead: Duration,
    /// One-way latency between entities.
    pub link_latency: Duration,
    /// Link bandwidth.
    pub link_bandwidth_bps: u64,
    /// The hardware profile costs are read from.
    pub profile: &'static str,
}

impl Default for EtsiParams {
    fn default() -> EtsiParams {
        EtsiParams {
            ec_validity: Duration::from_secs(3 * 365 * 24 * 60 * 60),
            at_validity: Duration::from_secs(7 * 24 * 60 * 60),
            at_concurrent: 100,
            at_preload: Duration::from_secs(90 * 24 * 60 * 60),
            subject_attributes_bytes: 64,
            servers: 4,
            overhead: Duration::from_millis(1),
            link_latency: Duration::from_millis(10),
            link_bandwidth_bps: 1_000_000_000,
            profile: profiles::I9_11950H_WOLFSSL,
        }
    }
}

/// The messages of the two built flows.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Ts102941Msg {
    /// ITS-S → EA: `InnerEcRequest` with a proof-of-possession inner signature.
    EnrolmentRequest {
        /// The station.
        station: NodeId,
    },
    /// EA → ITS-S: the enrolment credential.
    EnrolmentResponse {
        /// The station.
        station: NodeId,
        /// Whether the EA issued one.
        granted: bool,
    },
    /// ITS-S → AA: `InnerAtRequest`.
    AuthorizationRequest {
        /// The station.
        station: NodeId,
    },
    /// AA → EA: `AuthorizationValidationRequest`.
    ValidationRequest {
        /// The station, carried for the simulation's bookkeeping. The AA never learns it:
        /// it forwards the `ecSignature` encrypted to the EA and holds only the keyTag.
        station: NodeId,
    },
    /// EA → AA: `AuthorizationValidationResponse`.
    ValidationResponse {
        /// The station.
        station: NodeId,
        /// Whether the EA validated the enrolment credential.
        valid: bool,
    },
    /// AA → ITS-S: the authorization ticket.
    AuthorizationResponse {
        /// The station.
        station: NodeId,
        /// Whether one was issued.
        granted: bool,
    },
}

/// Hand-written sizes for the two flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EtsiSizes {
    certs: CertificateSizes,
    subject_attributes_bytes: u32,
}

impl EtsiSizes {
    /// The sizes for a deployment.
    pub const fn new(certs: CertificateSizes, subject_attributes_bytes: u32) -> EtsiSizes {
        EtsiSizes {
            certs,
            subject_attributes_bytes,
        }
    }

    /// `EnrolmentRequest`: `InnerEcRequest` + POP + outer signature, encrypted to the EA.
    pub const fn enrolment_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + HASHED_ID8_BYTES
                + EC_POINT_COMPRESSED_BYTES
                + self.subject_attributes_bytes
                + 2 * ECDSA_P256_SIG_COER_BYTES
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_subject_attributes_bytes",
        )
    }

    /// `EnrolmentResponse`: the EC under the EA's certificate.
    pub const fn enrolment_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + self.certs.enrolment.bytes()
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "envelope(certificate) + EA certificate + enrolment credential + ECIES wrapper",
            STRUCTURE,
        )
    }

    /// `AuthorizationRequest`: `InnerAtRequest` encrypted to the AA.
    pub const fn authorization_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + 2 * EC_POINT_COMPRESSED_BYTES
                + SHA256_BYTES
                + HASHED_ID8_BYTES
                + AES128_KEY_BYTES
                + self.subject_attributes_bytes
                + self.ec_signature_bytes()
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_subject_attributes_bytes",
        )
    }

    /// The `ecSignature` the AA cannot read: a signed, encrypted `SharedAtRequest` hash.
    const fn ec_signature_bytes(&self) -> u32 {
        ENVELOPE_OVERHEAD_DIGEST_BYTES
            + SHA256_BYTES
            + ECDSA_P256_SIG_COER_BYTES
            + ECIES_P256_ENCRYPTED_KEY_BYTES
    }

    /// `AuthorizationValidationRequest`.
    pub const fn validation_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + HASHED_ID8_BYTES
                + AES128_KEY_BYTES
                + self.subject_attributes_bytes
                + self.ec_signature_bytes()
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_subject_attributes_bytes",
        )
    }

    /// `AuthorizationValidationResponse`.
    pub const fn validation_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + 1
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "envelope(certificate) + EA certificate + response code + ECIES wrapper",
            STRUCTURE,
        )
    }

    /// `AuthorizationResponse`: the AT, encrypted to the station.
    pub const fn authorization_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + self.certs.pseudonym.bytes()
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "envelope(digest) + authorization ticket + ECIES wrapper",
            STRUCTURE,
        )
    }

    /// Every message kind, for the conformance test.
    pub fn table(&self) -> Vec<(&'static str, WireSize)> {
        vec![
            ("etsi-enrolment-request", self.enrolment_request()),
            ("etsi-enrolment-response", self.enrolment_response()),
            ("etsi-authorization-request", self.authorization_request()),
            ("etsi-validation-request", self.validation_request()),
            ("etsi-validation-response", self.validation_response()),
            ("etsi-authorization-response", self.authorization_response()),
        ]
    }
}

/// The flows this skeleton runs, and the stages they emit.
pub const FLOWS: &[FlowSpec] = &[
    FlowSpec {
        id: FlowId::EtsiEnrolment,
        participants: &["ITS-S", "EA"],
        stages: &[StageId::Requested, StageId::Certified, StageId::Installed],
    },
    FlowSpec {
        id: FlowId::EtsiAuthorization,
        participants: &["ITS-S", "AA", "EA"],
        stages: &[
            StageId::Requested,
            StageId::ProxyForwarded,
            StageId::Certified,
            StageId::Installed,
        ],
    },
];

/// Vehicles are revoked passively: the EA refuses subsequent AT requests and the internal
/// blacklist is never published [TS 102 941 §6.1.6; EUCP §7.3.2].
pub const PASSIVE_REVOCATION: PassiveRevocation = PassiveRevocation {
    blocklist_at: "EA",
    stages: &[
        StageId::Decision,
        StageId::Blocklisted,
        StageId::LastValidCredentialExpiry,
    ],
};

/// The separations TS 102 940 requires.
pub const SEPARATIONS: &[SeparationRule] = &[SeparationRule {
    a: "EA",
    b: "AA",
    reason: "the AA must not learn the enrolment identity and the EA must not learn the AT \
             keys [TS 102 941 §6.1.4 NOTE 1]",
}];

/// The ETSI skeleton.
#[derive(Debug)]
pub struct EtsiTs102941 {
    card: ModelCard,
    params: EtsiParams,
}

impl Default for EtsiTs102941 {
    fn default() -> EtsiTs102941 {
        EtsiTs102941::new(EtsiParams::default())
    }
}

impl EtsiTs102941 {
    /// The skeleton with `params`.
    pub fn new(params: EtsiParams) -> EtsiTs102941 {
        EtsiTs102941 {
            card: card(&params),
            params,
        }
    }

    /// The protocol's id.
    pub const fn protocol_id(&self) -> ProtocolId {
        ProtocolId(ETSI_TS102941_ID)
    }

    /// Its parameters.
    pub const fn params(&self) -> &EtsiParams {
        &self.params
    }

    /// The roles.
    pub fn roles(&self) -> Vec<EntityRoleSpec> {
        let svc = ServiceModelSpec::new(self.params.servers, self.params.overhead);
        ["EA", "AA", "RCA", "TLM", "CPOC", "MA"]
            .into_iter()
            .map(|name| EntityRoleSpec {
                name,
                boundary: match name {
                    "EA" => TrustBoundary::Registration,
                    "AA" => TrustBoundary::Issuer,
                    "MA" => TrustBoundary::Misbehaviour,
                    _ => TrustBoundary::Policy,
                },
                central: Centrality::Central,
                default_profile: self.params.profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: name == "RCA",
            })
            .collect()
    }

    /// The credential types.
    pub fn credential_types(&self) -> Vec<CredentialTypeSpec> {
        let p = &self.params;
        let certs = CertificateSizes::measured().ok();
        vec![
            CredentialTypeSpec {
                name: "enrolment-credential",
                encoded: certs.map_or(
                    WireSize::cited(117, "04-models.md §9.2 cross-check"),
                    |c| c.enrolment,
                ),
                validity: ValidityPolicy {
                    period: p.ec_validity,
                    overlap: Duration::ZERO,
                    concurrent: 1,
                    preload: Duration::ZERO,
                },
                holder: HolderKind::EndEntity,
                covers: Vec::new(),
            },
            CredentialTypeSpec {
                name: "authorization-ticket",
                encoded: certs.map_or(
                    WireSize::cited(80, "SEC 4 §3.4; 04-models.md §9.2"),
                    |c| c.pseudonym,
                ),
                validity: ValidityPolicy {
                    period: p.at_validity,
                    overlap: Duration::ZERO,
                    concurrent: p.at_concurrent,
                    preload: p.at_preload,
                },
                holder: HolderKind::EndEntity,
                covers: Vec::new(),
            },
        ]
    }

    /// The flows.
    pub const fn flows(&self) -> &'static [FlowSpec] {
        FLOWS
    }

    /// Revocation: passive only, for vehicles.
    pub const fn revocation(&self) -> RevocationMechanism {
        RevocationMechanism::Passive(PASSIVE_REVOCATION)
    }

    /// The worst-case eviction lag after the EA blocklists a station: the preload horizon
    /// plus the ticket validity [EUCP §7.2.1].
    ///
    /// This is the `last_valid_credential_expiry` bound of 05-protocols §2.5, computed
    /// rather than measured, because with no CRL for vehicles there is nothing to measure:
    /// the station keeps transmitting until its pool runs out.
    pub const fn passive_eviction_bound(&self) -> Duration {
        Duration::from_nanos(
            self.params
                .at_preload
                .as_nanos()
                .saturating_add(self.params.at_validity.as_nanos()),
        )
    }
}

impl Model for EtsiTs102941 {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// A running ETSI deployment.
pub struct EtsiRun {
    /// The roles' nodes.
    pub nodes: EtsiNodes,
    /// The parameters.
    pub params: EtsiParams,
    /// The sizes.
    pub sizes: EtsiSizes,
    /// Stations the EA has enrolled.
    pub enrolled: BTreeSet<NodeId>,
    /// Stations the EA has blocklisted (the passive revocation).
    pub blocklist: BTreeSet<NodeId>,
    /// Authorization tickets each station holds.
    pub tickets: BTreeMap<NodeId, u32>,
    /// AT requests the EA refused.
    pub refused: u32,
    /// The kernel.
    pub kernel: Kernel<Ts102941Msg>,
    next_run: u32,
}

impl EtsiRun {
    /// Builds the deployment.
    ///
    /// # Errors
    /// [`crate::error::ProtoError::Size`] if the certificate profile does not encode.
    pub fn new(params: EtsiParams) -> Result<EtsiRun> {
        let nodes = EtsiNodes::default();
        let sizes = EtsiSizes::new(
            CertificateSizes::measured()?,
            params.subject_attributes_bytes,
        );
        let mut net = BackendNet::new();
        let link = Link {
            latency: params.link_latency,
            bandwidth_bps: params.link_bandwidth_bps,
            transport: Transport::BackendNet,
        };
        for (a, b) in nodes.links() {
            net.connect(a, b, link);
        }
        let mut kernel = Kernel::new(net);
        let svc = ServiceModelSpec::new(params.servers, params.overhead);
        for node in [
            nodes.ea, nodes.aa, nodes.rca, nodes.tlm, nodes.cpoc, nodes.ma,
        ] {
            kernel.host(node, &svc, params.profile);
        }
        Ok(EtsiRun {
            nodes,
            params,
            sizes,
            enrolled: BTreeSet::new(),
            blocklist: BTreeSet::new(),
            tickets: BTreeMap::new(),
            refused: 0,
            kernel,
            next_run: 0,
        })
    }

    /// Adds a station with its links to the EA and the AA.
    pub fn add_station(&mut self, station: NodeId) {
        let link = Link {
            latency: self.params.link_latency,
            bandwidth_bps: self.params.link_bandwidth_bps,
            transport: Transport::CellularUu,
        };
        let (ea, aa) = (self.nodes.ea, self.nodes.aa);
        {
            let net = self.kernel.net_mut();
            net.connect(station, ea, link);
            net.connect(station, aa, link);
        }
        self.kernel.host(
            station,
            &ServiceModelSpec::new(1, self.params.overhead),
            self.params.profile,
        );
    }

    fn new_run(&mut self) -> FlowRun {
        self.next_run += 1;
        FlowRun(self.next_run)
    }

    fn inject(&mut self, to: NodeId, msg: Ts102941Msg, flow: FlowId, run: FlowRun) {
        let at = self.kernel.now();
        self.kernel.inject(
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

    /// Starts the enrolment flow.
    pub fn enrol(&mut self, station: NodeId) -> FlowRun {
        let run = self.new_run();
        self.inject(
            station,
            Ts102941Msg::EnrolmentRequest { station },
            FlowId::EtsiEnrolment,
            run,
        );
        run
    }

    /// Starts the authorization flow.
    pub fn authorize(&mut self, station: NodeId) -> FlowRun {
        let run = self.new_run();
        self.inject(
            station,
            Ts102941Msg::AuthorizationRequest { station },
            FlowId::EtsiAuthorization,
            run,
        );
        run
    }

    /// The EA's blocklist decision: the passive revocation of TS 102 941 §6.1.6.
    pub fn blocklist(&mut self, station: NodeId) {
        self.blocklist.insert(station);
    }

    /// Runs until nothing is scheduled.
    ///
    /// # Errors
    /// Whatever [`Kernel::dispatch`] returns.
    pub fn run(&mut self) -> Result<()> {
        while let Some(d) = self.kernel.next_delivery() {
            let profile = self.kernel.profile_of(d.to);
            let mut out = Outbox::new(profile);
            let (at, to) = (d.at, d.to);
            self.handle(d, &mut out);
            self.kernel.dispatch(at, to, out)?;
        }
        Ok(())
    }

    fn handle(&mut self, d: Delivery<Ts102941Msg>, out: &mut Outbox<Ts102941Msg>) {
        use v2xw_sec::primitive::{PrimitiveId, PrimitiveOpKind};
        let (flow, run, to) = (d.flow, d.run, d.to);
        let n = self.nodes;
        let sign = |out: &mut Outbox<Ts102941Msg>, k: u32| {
            out.charge(PrimitiveId::ECDSA_P256_SHA256, PrimitiveOpKind::Sign, k);
        };
        let verify = |out: &mut Outbox<Ts102941Msg>, k: u32| {
            out.charge(PrimitiveId::ECDSA_P256_SHA256, PrimitiveOpKind::Verify, k);
        };
        match d.msg {
            Ts102941Msg::EnrolmentRequest { station } if to == station => {
                // The inner proof-of-possession signature and the outer one.
                sign(out, 2);
                out.stage_at(StageId::Requested, station, None, flow, run);
                out.send(
                    n.ea,
                    Ts102941Msg::EnrolmentRequest { station },
                    "etsi-enrolment-request",
                    self.sizes.enrolment_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::EnrolmentRequest { station } => {
                verify(out, 2);
                sign(out, 1);
                let granted = !self.blocklist.contains(&station);
                if granted {
                    self.enrolled.insert(station);
                    out.stage_at(StageId::Certified, to, None, flow, run);
                }
                out.send(
                    station,
                    Ts102941Msg::EnrolmentResponse { station, granted },
                    "etsi-enrolment-response",
                    self.sizes.enrolment_response(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::EnrolmentResponse { station, granted } => {
                verify(out, 1);
                if granted {
                    out.stage_at(StageId::Installed, station, None, flow, run);
                }
            }
            Ts102941Msg::AuthorizationRequest { station } if to == station => {
                // A fresh key pair per ticket, an HMAC key tag, and the inner signature
                // the EA will check — the AA sees none of the keys [TS 102 941 §6.1.4].
                out.charge(PrimitiveId::ECDSA_P256_SHA256, PrimitiveOpKind::KeyGen, 1);
                sign(out, 2);
                out.stage_at(StageId::Requested, station, None, flow, run);
                out.send(
                    n.aa,
                    Ts102941Msg::AuthorizationRequest { station },
                    "etsi-authorization-request",
                    self.sizes.authorization_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::AuthorizationRequest { station } => {
                verify(out, 1);
                sign(out, 1);
                out.stage_at(StageId::ProxyForwarded, to, None, flow, run);
                out.send(
                    n.ea,
                    Ts102941Msg::ValidationRequest { station },
                    "etsi-validation-request",
                    self.sizes.validation_request(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ValidationRequest { station } => {
                verify(out, 2);
                sign(out, 1);
                let valid = self.enrolled.contains(&station) && !self.blocklist.contains(&station);
                if !valid {
                    self.refused += 1;
                }
                out.send(
                    n.aa,
                    Ts102941Msg::ValidationResponse { station, valid },
                    "etsi-validation-response",
                    self.sizes.validation_response(),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ValidationResponse { station, valid } => {
                verify(out, 1);
                if valid {
                    sign(out, 1);
                    out.stage_at(StageId::Certified, to, None, flow, run);
                }
                out.send(
                    station,
                    Ts102941Msg::AuthorizationResponse {
                        station,
                        granted: valid,
                    },
                    "etsi-authorization-response",
                    self.sizes.authorization_response(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::AuthorizationResponse { station, granted } => {
                verify(out, 1);
                if granted {
                    *self.tickets.entry(station).or_insert(0) += 1;
                    out.stage_at(StageId::Installed, station, None, flow, run);
                }
            }
        }
    }
}

fn card(p: &EtsiParams) -> ModelCard {
    let mut card = ModelCard::new(
        ETSI_TS102941_ID,
        Family::Protocol,
        "0.1.0",
        "ETSI TS 102 941 ITS PKI, skeleton: enrolment and standard authorization as flows \
         over queued Enrolment and Authorization Authorities, with hand-written message \
         structures because build decision D5 defers the ETSI PKI ASN.1.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![Equation::new(
        "passive eviction bound",
        "lag ≤ preload_horizon + ticket_validity (EUCP §7.2.1: ≤ 3 months + 1 week)",
    )];
    card.parameters = vec![
        Parameter::new(
            "ec_validity_days",
            "d",
            serde_json::json!(p.ec_validity.as_nanos() / (86_400 * 1_000_000_000)),
            Source::new(
                SourceKind::Standard,
                "EU C-ITS Certificate Policy 1.1 Table 11",
            ),
        ),
        Parameter::new(
            "at_validity_days",
            "d",
            serde_json::json!(p.at_validity.as_nanos() / (86_400 * 1_000_000_000)),
            Source::new(
                SourceKind::Standard,
                "EU C-ITS Certificate Policy 1.1 §7.2.1",
            ),
        ),
        Parameter::new(
            "at_concurrent",
            "-",
            serde_json::json!(p.at_concurrent),
            Source::new(
                SourceKind::Standard,
                "EU C-ITS Certificate Policy 1.1 §7.2.1 (≤ 100); ETSI TR 103 415 Table A.2 \
                 gives 20 parallel in the C2C-CC profile",
            ),
        ),
        Parameter::new(
            "at_preload_days",
            "d",
            serde_json::json!(p.at_preload.as_nanos() / (86_400 * 1_000_000_000)),
            Source::new(
                SourceKind::Standard,
                "EU C-ITS Certificate Policy 1.1 §7.2.1",
            ),
        ),
        {
            let mut param = Parameter::new(
                "etsi_subject_attributes_bytes",
                "B",
                serde_json::json!(p.subject_attributes_bytes),
                Source::todo_calibrate("no clause fixes the container's size"),
            );
            param.calibration = Some(
                "Size it from the real encoder once build decision D5's inner-subtyping \
                 failure in the ETSI PKI ASN.1 is resolved; until then it is a scenario \
                 parameter and every message that contains it is marked as resting on it."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "backend_servers",
                "-",
                serde_json::json!(p.servers),
                Source::todo_calibrate("no source picks a deployment size"),
            );
            param.calibration =
                Some("The 'c' of 06-node-models.md §4's M/M/c; set it per scenario.".into());
            param
        },
        {
            let mut param = Parameter::new(
                "link_latency_ms",
                "ms",
                serde_json::json!(p.link_latency.as_nanos() / 1_000_000),
                Source::todo_calibrate("no published EA/AA topology"),
            );
            param.calibration = Some(
                "TS 102 941 §6.2.2 fixes the transport (HTTP over TCP/IP, no TLS) but no \
                 latency; take it from the deployment being modelled."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "overhead_us",
                "us",
                serde_json::json!(p.overhead.as_nanos() / 1_000),
                Source::todo_calibrate("no published EA/AA transaction rates"),
            );
            param.calibration = Some(
                "Per-request overhead beyond the cryptography; measure against a reference \
                 EA/AA implementation."
                    .into(),
            );
            param
        },
    ];
    card.assumptions = vec![
        "There is no per-vehicle CRL: TS 102 941 §6.1.4 NOTE 4 states that revocation of \
         authorization tickets is not possible because passive revocation is preferred."
            .into(),
    ];
    card.limitations = vec![
        "A skeleton. The butterfly authorization variant (§6.2.3.5), the ECTL/CTL trust-list \
         flows (§6.3) and the TS 103 759 reporting path are declared and not built."
            .into(),
        "Message sizes are hand-written sums of cited field sizes, not encoder output, \
         because build decision D5 defers the ETSI PKI ASN.1 on an inner subtyping construct."
            .into(),
    ];
    card.sources = vec![
        Source::new(SourceKind::Standard, "ETSI TS 102 941 V2.2.1 (2022-11)"),
        Source::new(SourceKind::Standard, "ETSI TS 102 940 V2.1.1 (2021-07)"),
        Source::new(SourceKind::Standard, "ETSI TS 103 097 V2.1.1 (2021-10)"),
        Source::new(
            SourceKind::Standard,
            "European Commission, Certificate Policy for Deployment and Operation of European \
             C-ITS, Release 1.1, 2018-06",
        ),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.determinism = Determinism::default();
    card
}
