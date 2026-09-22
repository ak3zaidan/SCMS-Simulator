//! `protocol/scms/camp` — the CAMP SCMS / IEEE 1609.2.1 credential-management protocol.
//!
//! 05-protocols.md §3 is the specification. The parts of it that are *structure* are here:
//! the roles and the separations between them, the credential types, the flows and the
//! stages each is required to emit, and the revocation mechanism. The parts that are
//! *behaviour* are in [`run`], where each role is a node with a queue and each flow is a
//! sequence of messages across modelled links.
//!
//! What this plug-in is for, in one sentence: to measure what credential management costs
//! and what it leaks. That is why the Registration Authority is a queue rather than a
//! function, why a pre-linkage value is a type the RA cannot open, and why a Linkage
//! Authority releases `ls(i)` and never `ls(0)`.

pub mod msg;
pub mod params;
pub mod run;

use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::time::Duration;
use v2xw_sec::primitive::PrimitiveId;

use crate::sizes::WireSize;
use crate::sizes::{CRL_LINKAGE_ENTRY_BYTES, CertificateSizes, SizeParams};
use crate::spec::{
    ActiveRevocation, Centrality, CredentialTypeSpec, EntityRoleSpec, FlowSpec, HolderKind,
    PassiveRevocation, ProtocolId, RevocationMechanism, SeparationRule, StorageCounter,
    TrustBoundary, ValidityPolicy,
};
use crate::stage::{FlowId, StageId};

pub use params::{ScmsNodes, ScmsParams};
pub use run::{ScmsRun, ScmsState};

/// The plug-in's stable id.
pub const CAMP_SCMS_ID: &str = "protocol/scms/camp";

/// The separations [BRECHT §X] declares mandatory.
///
/// A scenario that co-hosts a pair is refused unless it sets
/// `security.protocol.params.relax_separation` — the check is
/// [`crate::spec::separation_violations`], which runs against a placement.
pub const SEPARATIONS: &[SeparationRule] = &[
    SeparationRule {
        a: "PCA",
        b: "RA",
        reason: "the PCA must not learn which request a certificate came from [BRECHT §X]",
    },
    SeparationRule {
        a: "PCA",
        b: "LA1",
        reason: "the PCA must not be able to compute a linkage value from a seed [BRECHT §X]",
    },
    SeparationRule {
        a: "PCA",
        b: "LA2",
        reason: "the PCA must not be able to compute a linkage value from a seed [BRECHT §X]",
    },
    SeparationRule {
        a: "LA1",
        b: "LA2",
        reason: "one LA holding both chains could link a device unilaterally [BRECHT §X]",
    },
    SeparationRule {
        a: "LOP",
        b: "RA",
        reason: "the proxy exists to hide the device's network identity from the RA [BRECHT §X]",
    },
    SeparationRule {
        a: "LOP",
        b: "MA",
        reason: "the proxy must not be the party investigating [BRECHT §X]",
    },
    SeparationRule {
        a: "MA",
        b: "RA",
        reason: "investigation must require a request the RA answers, not one it makes [BRECHT §X]",
    },
    SeparationRule {
        a: "MA",
        b: "LA1",
        reason: "the MA must have to ask for a seed [BRECHT §X]",
    },
    SeparationRule {
        a: "MA",
        b: "LA2",
        reason: "the MA must have to ask for a seed [BRECHT §X]",
    },
    SeparationRule {
        a: "MA",
        b: "PCA",
        reason: "the MA must have to ask for the chain identifiers [BRECHT §X]",
    },
];

/// The flows, and the stages each is required to emit.
///
/// `tests/flows.rs` runs every one of these and asserts the stamped stages equal the list
/// here, in order. The lists are therefore executable specification, not commentary.
pub const FLOWS: &[FlowSpec] = &[
    FlowSpec {
        id: FlowId::Enrolment,
        participants: &["EE", "DCM", "ECA"],
        stages: &[
            StageId::Requested,
            StageId::ProxyForwarded,
            StageId::Certified,
            StageId::Installed,
        ],
    },
    FlowSpec {
        id: FlowId::Provisioning,
        participants: &["EE", "LOP", "RA", "LA1", "LA2", "PCA"],
        stages: PROVISIONING_STAGES,
    },
    FlowSpec {
        id: FlowId::Topup,
        participants: &["EE", "LOP", "RA", "LA1", "LA2", "PCA"],
        stages: PROVISIONING_STAGES,
    },
    FlowSpec {
        id: FlowId::Report,
        participants: &["EE", "LOP", "RA", "MA"],
        stages: &[
            StageId::Detect,
            StageId::ReportSent,
            StageId::ProxyForwarded,
            StageId::Shuffled,
            StageId::ReportReceived,
        ],
    },
    FlowSpec {
        id: FlowId::LinkageResolution,
        participants: &["MA", "PCA", "LA1", "LA2", "RA"],
        stages: &[StageId::Decision, StageId::Resolved, StageId::Blocklisted],
    },
    FlowSpec {
        id: FlowId::CrlIssuance,
        participants: &["MA", "CRLG", "CRL Store", "CRL Broadcast"],
        stages: &[
            StageId::Issued,
            StageId::Published,
            StageId::FirstRsuBroadcast,
        ],
    },
    FlowSpec {
        id: FlowId::CrlDistribution,
        participants: &["CRL Store", "EE"],
        stages: &[StageId::Downloaded, StageId::Processed, StageId::Enforced],
    },
];

/// Top-up is the provisioning state machine with a one-period request, so it emits the
/// same stages ([PRIMER pp.5,7]; [CAMP-EE §2.2.7.7.7]).
const PROVISIONING_STAGES: &[StageId] = &[
    StageId::Requested,
    StageId::ProxyForwarded,
    StageId::Acknowledged,
    StageId::Expanded,
    StageId::PreLinkageReady,
    StageId::Shuffled,
    StageId::Certified,
    StageId::BatchReady,
    StageId::Downloaded,
    StageId::Installed,
];

/// The active half of SCMS revocation: a linked CRL in series 1, daily by default.
pub const ACTIVE_REVOCATION: ActiveRevocation = ActiveRevocation {
    entry: WireSize::cited(
        CRL_LINKAGE_ENTRY_BYTES,
        "Brecht 2018 §VI-F: 32 B of seeds plus group overhead, ≈ 40 B per entry",
    ),
    series: 1,
    cadence: Duration::from_secs(86_400),
    stages: &[
        StageId::Decision,
        StageId::Issued,
        StageId::Published,
        StageId::FirstRsuBroadcast,
        StageId::Downloaded,
        StageId::Processed,
        StageId::Enforced,
    ],
};

/// The passive half: the RA stops issuing to a blocklisted enrolment certificate.
pub const PASSIVE_REVOCATION: PassiveRevocation = PassiveRevocation {
    blocklist_at: "RA",
    stages: &[
        StageId::Decision,
        StageId::Blocklisted,
        StageId::LastValidCredentialExpiry,
    ],
};

/// The CAMP SCMS plug-in.
#[derive(Debug)]
pub struct CampScms {
    card: ModelCard,
    params: ScmsParams,
}

impl Default for CampScms {
    fn default() -> CampScms {
        CampScms::new(ScmsParams::default())
    }
}

impl CampScms {
    /// The plug-in with `params`.
    pub fn new(params: ScmsParams) -> CampScms {
        CampScms {
            card: card(&params),
            params,
        }
    }

    /// The protocol's id.
    pub const fn protocol_id(&self) -> ProtocolId {
        ProtocolId(CAMP_SCMS_ID)
    }

    /// Its parameters.
    pub const fn params(&self) -> &ScmsParams {
        &self.params
    }

    /// The backend roles.
    pub fn roles(&self) -> Vec<EntityRoleSpec> {
        let p = &self.params;
        let svc = crate::service::ServiceModelSpec::new(p.backend_servers, p.backend_overhead);
        let shuffling = svc.batched(crate::service::BatchPolicy::CAMP_SHUFFLE);
        let cert = CertificateSizes::measured().ok();
        let pseudonym = cert.map_or(
            WireSize::cited(80, "SEC 4 §3.4: implicit certificate, 04-models.md §9.2"),
            |c| c.pseudonym,
        );
        let enrolment = cert.map_or(
            WireSize::cited(117, "04-models.md §9.2 cross-check (VSC-A)"),
            |c| c.enrolment,
        );
        vec![
            EntityRoleSpec {
                name: "RA",
                boundary: TrustBoundary::Registration,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: shuffling,
                storage_growth: vec![
                    StorageCounter {
                        what: "enrolment certificate per device [BRECHT Table II]",
                        bytes_each: enrolment,
                    },
                    StorageCounter {
                        what: "request hash per provisioning request [BRECHT Table II]",
                        bytes_each: WireSize::cited(32, "FIPS 180-4: SHA-256 digest"),
                    },
                ],
                offline: false,
            },
            EntityRoleSpec {
                name: "PCA",
                boundary: TrustBoundary::Issuer,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: vec![StorageCounter {
                    what: "(i, j, lv, certificate, request hash) per issued certificate \
                           [BRECHT Table II]",
                    bytes_each: pseudonym,
                }],
                offline: false,
            },
            la_role("LA1", TrustBoundary::Linkage(1), p, svc),
            la_role("LA2", TrustBoundary::Linkage(2), p, svc),
            EntityRoleSpec {
                name: "MA",
                boundary: TrustBoundary::Misbehaviour,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: vec![StorageCounter {
                    what: "misbehaviour report [TS 103 759 §5.1]",
                    bytes_each: WireSize::parameter(
                        p.sizes.report_payload_bytes,
                        "report_payload_bytes",
                    ),
                }],
                offline: false,
            },
            EntityRoleSpec {
                name: "CRLG",
                boundary: TrustBoundary::Revocation,
                central: Centrality::IntrinsicallyCentral,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: vec![StorageCounter {
                    what: "CRL entry [BRECHT §VI-F]",
                    bytes_each: ACTIVE_REVOCATION.entry,
                }],
                offline: false,
            },
            EntityRoleSpec {
                name: "LOP",
                boundary: TrustBoundary::Proxy,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: false,
            },
            EntityRoleSpec {
                name: "CRL Store",
                boundary: TrustBoundary::Revocation,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: vec![StorageCounter {
                    what: "CRL entry [BRECHT §VI-F]",
                    bytes_each: ACTIVE_REVOCATION.entry,
                }],
                offline: false,
            },
            EntityRoleSpec {
                name: "CRL Broadcast",
                boundary: TrustBoundary::Revocation,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: false,
            },
            EntityRoleSpec {
                name: "ECA",
                boundary: TrustBoundary::Issuer,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: vec![StorageCounter {
                    what: "enrolment certificate [BRECHT Table II]",
                    bytes_each: enrolment,
                }],
                offline: false,
            },
            EntityRoleSpec {
                name: "DCM",
                boundary: TrustBoundary::Registration,
                central: Centrality::Central,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: false,
            },
            EntityRoleSpec {
                name: "Root CA",
                boundary: TrustBoundary::Policy,
                central: Centrality::IntrinsicallyCentral,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: true,
            },
            EntityRoleSpec {
                name: "Policy Generator",
                boundary: TrustBoundary::Policy,
                central: Centrality::IntrinsicallyCentral,
                default_profile: p.backend_profile,
                default_service: svc,
                storage_growth: Vec::new(),
                offline: true,
            },
        ]
    }

    /// The credential types.
    pub fn credential_types(&self) -> Vec<CredentialTypeSpec> {
        let p = &self.params;
        let cert = CertificateSizes::measured().ok();
        vec![
            CredentialTypeSpec {
                name: "pseudonym-cert",
                encoded: cert.map_or(
                    WireSize::cited(80, "SEC 4 §3.4; 04-models.md §9.2"),
                    |c| c.pseudonym,
                ),
                validity: ValidityPolicy {
                    period: p.i_period,
                    overlap: Duration::from_secs(60 * 60),
                    concurrent: p.certs_per_period,
                    preload: Duration::from_secs(3 * 52 * 7 * 24 * 60 * 60),
                },
                holder: HolderKind::EndEntity,
                covers: Vec::new(),
            },
            CredentialTypeSpec {
                name: "enrollment-cert",
                encoded: cert.map_or(
                    WireSize::cited(117, "04-models.md §9.2 cross-check (VSC-A)"),
                    |c| c.enrolment,
                ),
                validity: ValidityPolicy {
                    period: Duration::from_secs(7 * 365 * 24 * 60 * 60),
                    overlap: Duration::ZERO,
                    concurrent: 1,
                    preload: Duration::ZERO,
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

    /// The separations this protocol requires.
    pub const fn separations(&self) -> &'static [SeparationRule] {
        SEPARATIONS
    }

    /// Revocation: both halves.
    pub const fn revocation(&self) -> RevocationMechanism {
        RevocationMechanism::Both(ACTIVE_REVOCATION, PASSIVE_REVOCATION)
    }

    /// The primitives the protocol uses.
    pub fn primitives(&self) -> Vec<PrimitiveId> {
        vec![
            PrimitiveId::ECDSA_P256_SHA256,
            PrimitiveId::ECQV_P256,
            PrimitiveId::SHA_256,
            crate::spec::AES128_BLOCK,
        ]
    }
}

fn la_role(
    name: &'static str,
    boundary: TrustBoundary,
    p: &ScmsParams,
    svc: crate::service::ServiceModelSpec,
) -> EntityRoleSpec {
    EntityRoleSpec {
        name,
        boundary,
        central: Centrality::Central,
        default_profile: p.backend_profile,
        default_service: svc,
        storage_growth: vec![StorageCounter {
            what: "initial seed and pre-linkage values per device [BRECHT Table II]",
            bytes_each: WireSize::cited(16, "Ieee1609Dot2BaseTypes.asn: LinkageSeed is 16 B"),
        }],
        offline: false,
    }
}

impl Model for CampScms {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

fn std_src(reference: &'static str) -> Source {
    Source::new(SourceKind::Standard, reference)
}

fn paper(reference: &'static str) -> Source {
    Source::new(SourceKind::Paper, reference)
}

fn todo(
    name: &'static str,
    unit: &'static str,
    default: serde_json::Value,
    plan: &'static str,
) -> Parameter {
    let mut p = Parameter::new(
        name,
        unit,
        default,
        Source::todo_calibrate("no published value in the design set"),
    );
    p.calibration = Some(plan.to_string());
    p
}

fn card(p: &ScmsParams) -> ModelCard {
    let mut card = ModelCard::new(
        CAMP_SCMS_ID,
        Family::Protocol,
        "1.0.0",
        "The CAMP SCMS / IEEE 1609.2.1 credential-management protocol: nine online backend \
         roles as queued nodes, butterfly-key pseudonym provisioning, two-Linkage-Authority \
         identity resolution, and linked-CRL revocation that is forward-only by construction.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation::new(
            "butterfly expansion",
            "B(i,j) = A + f1(ck,(i,j))·G ;  Q(i,j) = P + f2(ek,(i,j))·G ;  \
             b'(i,j) = a + f1(ck,(i,j)) + c",
        ),
        Equation::new(
            "linkage value",
            "lv(i,j) = plv1(i,j) XOR plv2(i,j), plv_x(i,j) = AES_{ls_x(i)}(la_id_x ‖ j) truncated \
             to 72 bits",
        ),
        Equation::new(
            "seed chain",
            "ls_x(i) = H(la_id_x ‖ ls_x(i-1)) truncated to 128 bits; a CRL entry publishes \
             ls_x(i_rev) and never ls_x(0), which is what makes revocation forward-only",
        ),
        Equation::new(
            "CRL processing cost",
            "per entry per i-period: 2 SHA-256 chain steps and 2·jmax AES blocks",
        ),
        Equation::new(
            "link delay",
            "t = latency + 8·bytes / bandwidth, in integer nanoseconds",
        ),
    ];
    card.parameters = vec![
        Parameter::new(
            "i_period_minutes",
            "min",
            serde_json::json!(10_080),
            std_src("CAMP SCMS PoC EE Requirements R1.1 §2.1.5.3.2"),
        ),
        Parameter::new(
            "cert_lifetime_minutes",
            "min",
            serde_json::json!(10_140),
            std_src("CAMP SCMS PoC EE Requirements R1.1 §2.1.5.3.2 (one hour of overlap)"),
        ),
        Parameter::new(
            "certs_per_period",
            "-",
            serde_json::json!(p.certs_per_period),
            std_src("CAMP SCMS PoC EE Requirements R1.1 Table 2.1.2.6.2"),
        ),
        Parameter::new(
            "initial_batch_certs",
            "-",
            serde_json::json!(p.initial_batch),
            paper("USDOT SCMS Technical Primer (FHWA-JPO-19-775) p.7: 3,120 = 20 × 52 × 3"),
        ),
        Parameter::new(
            "shuffle_threshold_requests",
            "-",
            serde_json::json!(10_000),
            std_src("CAMP SCMS PoC EE Requirements R1.1 §2.2.7"),
        ),
        Parameter::new(
            "shuffle_window_s",
            "s",
            serde_json::json!(86_400),
            std_src("CAMP SCMS PoC EE Requirements R1.1 §2.2.7: 10,000 requests or one day"),
        ),
        Parameter::new(
            "report_shuffle_threshold",
            "-",
            serde_json::json!(10_000),
            std_src("CAMP SCMS PoC EE Requirements R1.1 SCMS-765"),
        ),
        Parameter::new(
            "crl_cadence_s",
            "s",
            serde_json::json!(86_400),
            paper("Brecht 2018 §VI-G; USDOT 2013 daily working assumption"),
        ),
        Parameter::new(
            "crl_entry_bytes",
            "B",
            serde_json::json!(CRL_LINKAGE_ENTRY_BYTES),
            paper("Brecht 2018 §VI-F: 32 B of seeds plus group overhead"),
        ),
        Parameter::new(
            "crl_max_forward_periods",
            "-",
            serde_json::json!(v2xw_sec::linkage::DEFAULT_MAX_FORWARD_PERIODS),
            paper(
                "04-models.md §9.6: the CAMP end-entity certificate lifetime, 156 weekly \
                 periods; also a denial-of-service bound, see v2xw-sec's linkage module",
            ),
        ),
        Parameter::new(
            "pseudonym_change_interval_s",
            "s",
            serde_json::json!(300),
            paper("USDOT SCMS Technical Primer pp.7-8; SAE J2945/1 CERTCHG"),
        ),
        Parameter::new(
            "pseudonym_change_distance_m",
            "m",
            serde_json::json!(2_000),
            paper("USDOT SCMS Technical Primer pp.7-8 (NYC pilot: 2 km or 5 min)"),
        ),
        Parameter::new(
            "cert_attach_interval_ms",
            "ms",
            serde_json::json!(450),
            paper("Rostami et al. 2018 Table 1 (SAE J2945/1 CertAttachInt)"),
        ),
        Parameter::new(
            "backend_hw_profile",
            "-",
            serde_json::json!(p.backend_profile),
            Source::new(
                SourceKind::Datasheet,
                "wolfSSL benchmark, Intel i9-11950H [R5 §B.5]; 06-node-models.md §7.7 leaves \
                 appliance-level RA/PCA/MA transaction rates NOT PUBLISHED, so the service \
                 model composes them from these per-operation costs",
            ),
        ),
        Parameter::new(
            "device_hw_profile",
            "-",
            serde_json::json!(p.device_profile),
            Source::new(
                SourceKind::Datasheet,
                "Cohda MK6 with Botan [R5 §B.3]; 06-node-models.md §7.3",
            ),
        ),
        todo(
            "backend_servers",
            "-",
            serde_json::json!(p.backend_servers),
            "The 'c' of 06-node-models.md §4's M/M/c. No source picks a VM size; set it per \
             scenario and record it in the manifest.",
        ),
        todo(
            "backend_overhead_us",
            "us",
            serde_json::json!(p.backend_overhead.as_nanos() / 1_000),
            "Per-request overhead beyond the cryptography. 06-node-models.md §7.7 records \
             RA/PCA/MA transaction rates as NOT PUBLISHED; measure against a reference SCMS \
             deployment or an emulated RA and replace.",
        ),
        todo(
            "device_overhead_us",
            "us",
            serde_json::json!(p.device_overhead.as_nanos() / 1_000),
            "Per-request overhead on the OBU outside the crypto engine; measure on the \
             reference OBU profile of 06-node-models.md §7.2.",
        ),
        todo(
            "backend_link_latency_ms",
            "ms",
            serde_json::json!(p.backend_link_latency.as_nanos() / 1_000_000),
            "One-way latency between backend entities. 04-models.md §10 has no SCMS backend \
             topology; take it from the deployment being modelled, or from a WAN RTT \
             measurement between the regions the entities sit in.",
        ),
        todo(
            "backend_link_bandwidth_bps",
            "bit/s",
            serde_json::json!(p.backend_link_bandwidth_bps),
            "Backend link capacity; set from the deployment's provisioned bandwidth.",
        ),
        todo(
            "uu_link_latency_ms",
            "ms",
            serde_json::json!(p.uu_link_latency.as_nanos() / 1_000_000),
            "Device-to-backend one-way latency over cellular; replace with the measured \
             latency distribution of 04-models.md §10's medium-tier cellular model once that \
             model is wired to this plug-in.",
        ),
        todo(
            "uu_link_bandwidth_bps",
            "bit/s",
            serde_json::json!(p.uu_link_bandwidth_bps),
            "Cellular uplink capacity; same source as uu_link_latency_ms.",
        ),
        todo(
            "first_batch_delay_s",
            "s",
            serde_json::json!(p.first_batch_delay.as_nanos() / 1_000_000_000),
            "The 'first batch time' the RA returns in its acknowledgement. CAMP-EE §2.2.7.6 \
             makes it deployment-specific; take it from the RA being modelled.",
        ),
        todo(
            "download_poll_interval_s",
            "s",
            serde_json::json!(p.download_poll_interval.as_nanos() / 1_000_000_000),
            "How often a device re-reads X.info when the batch is not ready. CAMP-EE \
             §2.2.7.8.8 describes the polling but publishes no interval.",
        ),
        todo(
            "max_download_polls",
            "-",
            serde_json::json!(p.max_download_polls),
            "A termination bound, not a protocol constant: a device that polls forever would \
             hang a run. Set it above the worst-case batch delay of the scenario.",
        ),
        todo(
            "download_horizon_weeks",
            "week",
            serde_json::json!(4),
            "How far ahead a device keeps its pool topped up. 05-protocols.md §3.2 marks this \
             OEM-configured; take it from the deployment or sweep it.",
        ),
        todo(
            "repo_url_bytes",
            "B",
            serde_json::json!(p.sizes.repo_url_bytes),
            "The certificate-repository URL in the RA's acknowledgement. Measure one from a \
             reference RA, or drop the field if the deployment derives the URL.",
        ),
        todo(
            "batch_container_bytes",
            "B",
            serde_json::json!(p.sizes.batch_container_bytes),
            "Framing the X_Y.zip container adds around one i-period of certificates. Measure \
             against a real batch file.",
        ),
        todo(
            "report_payload_bytes",
            "B",
            serde_json::json!(p.sizes.report_payload_bytes),
            "The TS 103 759 EtsiTs103759Data payload. Build decision D5 defers the ETSI PKI \
             ASN.1, so no encoder can size it yet; once the inner-subtyping defect is resolved \
             this becomes a real-encoder size like every certificate here.",
        ),
        todo(
            "linkage_chain_identifier_bytes",
            "B",
            serde_json::json!(p.sizes.linkage_chain_identifier_bytes),
            "The LCI as it crosses RA-to-LA and MA-to-LA. CAMP publishes the concept but not \
             the encoding; take it from the 1609.2.1 upload interface definition.",
        ),
        todo(
            "etsi_subject_attributes_bytes",
            "B",
            serde_json::json!(p.sizes.etsi_subject_attributes_bytes),
            "The InnerEcRequest subject-attribute block; size it from the real encoder once \
             D5's ASN.1 defect is resolved.",
        ),
    ];
    card.assumptions = vec![
        "Root CA, the Policy Generator and the electors are offline, as in the CAMP PoC \
         (05-protocols.md §3.1), so they place no messages on any link."
            .into(),
        "Service times are deterministic sums of per-operation costs plus a fixed overhead: \
         the `medium` tier of 06-node-models.md §4 without its stochastic variant, whose \
         log-normal parameters the design leaves todo-calibrate."
            .into(),
        "SHA-256 and AES-128 have no cost anchor on any hardware profile in 04-models.md \
         §9.4, so their operations are counted and charged zero time rather than given an \
         invented rate."
            .into(),
    ];
    card.limitations = vec![
        "Availability (entity outages) is a declared hook and is not modelled: \
         06-node-models.md §4 leaves the two-state Markov parameters todo-calibrate."
            .into(),
        "Epidemic V2V CRL exchange and RSU-relayed provisioning are not built; the CRL \
         broadcast path is modelled as one node so that `first_rsu_broadcast` has a real \
         timestamp, but the air interface itself belongs to the radio crates."
            .into(),
        "The P2PCD and certificate-attachment policies are declared as parameters here and \
         enforced by the envelope in `v2xw-sec`, not by this crate."
            .into(),
    ];
    card.ignores = vec![
        "Certificate encryption is modelled by its size and its cost, not by encrypting: \
         the crypto-mode equivalence guarantee (I-S1) makes the two indistinguishable in the \
         event log."
            .into(),
    ];
    card.sources = vec![
        paper(
            "Brecht et al., 'A Security Credential Management System for V2X Communications', IEEE T-ITS 2018, arXiv:1802.05323",
        ),
        std_src(
            "CAMP VSC5, SCMS PoC Implementation EE Requirements and Specifications, Release 1.1, 2016-05-04",
        ),
        paper("USDOT ITS-JPO, SCMS Technical Primer, FHWA-JPO-19-775, 2019"),
        std_src("IEEE 1609.2-2016 and 1609.2a-2017"),
        std_src("ETSI TS 103 759 V2.1.1 (misbehaviour report payload)"),
        paper("Simplicio et al., ACPC, ePrint 2018/324 §2 (CRL expansion cost)"),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["crypto".into()],
    };
    card
}

/// Registers this crate's protocol models.
///
/// # Errors
/// Whatever the registry returns: a card that does not validate, or an id already taken.
pub fn register(
    registry: &mut v2xw_core::registry::Registry,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    use std::sync::Arc;
    Ok(vec![
        registry.register_model(Arc::new(CampScms::default()))?,
    ])
}

/// The five uncited wire sizes, re-exported so a card reader can find them.
pub const UNCITED_SIZE_PARAMETERS: [&str; 5] = SizeParams::NAMES;
