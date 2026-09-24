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
    HASHED_ID8_BYTES, SHA256_BYTES, TIME32_BYTES, WireSize,
};
use crate::spec::{
    ActiveRevocation, Centrality, CredentialTypeSpec, EntityRoleSpec, FlowSpec, HolderKind,
    PassiveRevocation, ProtocolId, RevocationMechanism, SeparationRule, TrustBoundary,
    ValidityPolicy,
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
    /// The backend links the built flows need.
    ///
    /// `(RCA, CPOC)` and `(MA, CPOC)` are here for the trust-list and CA-CRL flows: the
    /// Root CA publishes its CTL and the CA-only CRL through the Central Point of Contact
    /// [TS 102 941 §6.3.1-6.3.5], and invariant I-P1 refuses a message between two nodes
    /// with no link rather than delivering it for free.
    pub fn links(&self) -> Vec<(NodeId, NodeId)> {
        vec![
            (self.aa, self.ea),
            (self.rca, self.ea),
            (self.rca, self.aa),
            (self.rca, self.cpoc),
            (self.rca, self.tlm),
            (self.tlm, self.cpoc),
            (self.cpoc, self.ea),
            (self.cpoc, self.aa),
            (self.ma, self.ea),
            (self.ma, self.aa),
            (self.ma, self.cpoc),
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
    /// The `currentI` / `requestHash` / `nextDlTime` block of a
    /// `ButterflyAuthorizationResponse` — not fixed by any clause this build can read.
    pub butterfly_response_bytes: u32,
    /// How many authorization tickets the EA expands a caterpillar pair into, and
    /// therefore how many the AA certifies in one batch.
    pub butterfly_batch: u32,
    /// How many tickets one `ButterflyAtDownloadRequest` collects.
    pub at_download_batch: u32,
    /// The trust-list framing — `nextUpdate`, `ctlSequence` and the list container —
    /// which build decision D5's deferred ASN.1 is the only thing that would fix.
    pub ctl_framing_bytes: u32,
    /// How many certificate-authority entries the European Certificate Trust List holds.
    pub ctl_entries: u32,
    /// How many certificate-authority entries the Root CA's CA-only CRL holds.
    pub ca_crl_entries: u32,
    /// The TS 103 759 misbehaviour-report payload, before the envelope.
    pub report_payload_bytes: u32,
    /// Whether the Misbehaviour Authority runs the optional pre-processing stage of
    /// TS 103 759 §4 before collection.
    pub report_pre_processing: bool,
    /// How long a trust list is valid for: the `nextUpdate` horizon.
    pub ctl_validity: Duration,
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
            butterfly_response_bytes: 16,
            // §6.2.3.5 gives the shape and no count; the EU Certificate Policy caps the
            // *concurrent* pool at 100 and the C2C-CC profile at 20 [EUCP §7.2.1;
            // TR 103 415 Table A.2], so one batch is modelled as the C2C-CC pool and the
            // card records that as a choice rather than as a clause.
            butterfly_batch: 20,
            at_download_batch: 20,
            ctl_framing_bytes: 8,
            ctl_entries: 8,
            ca_crl_entries: 1,
            report_payload_bytes: 1_200,
            report_pre_processing: true,
            // "update cadence <= 3 months, stations updated within 1 week" [EUCP §2.2,
            // via 05-protocols.md §2.6]. Three months is the horizon; the week is the
            // station's obligation and not the list's validity.
            ctl_validity: Duration::from_secs(90 * 24 * 60 * 60),
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

    // --- §6.2.3.5, the butterfly variant -------------------------------------------
    /// ITS-S → EA: one `ButterflyAuthorizationRequest` carrying a new caterpillar pair.
    ButterflyAuthorizationRequest {
        /// The station.
        station: NodeId,
    },
    /// EA → ITS-S: `currentI`, `requestHash` and `nextDlTime`.
    ///
    /// The acknowledgement, not the tickets: the station learns *when* its batch will be
    /// downloadable and comes back for it, which is what makes the download a separate
    /// flow with its own latency.
    ButterflyAcknowledgement {
        /// The station.
        station: NodeId,
        /// The i-period the EA expanded for.
        current_i: u32,
        /// Whether the EA accepted the request at all.
        granted: bool,
    },
    /// EA → AA: one `ButterflyCertRequest` per expanded cocoon key, batched.
    ButterflyCertRequest {
        /// The station the batch belongs to. The AA never learns the enrolment identity;
        /// this is the simulation's bookkeeping, exactly as on
        /// [`Ts102941Msg::ValidationRequest`].
        station: NodeId,
        /// The i-period.
        current_i: u32,
        /// How many cocoon keys are in the batch.
        count: u32,
    },
    /// AA → EA: the certified tickets, each encrypted to the station.
    ButterflyCertResponse {
        /// The station.
        station: NodeId,
        /// The i-period.
        current_i: u32,
        /// How many were certified.
        count: u32,
    },
    /// ITS-S → EA: `ButterflyAtDownloadRequest`.
    ButterflyAtDownloadRequest {
        /// The station.
        station: NodeId,
        /// The i-period being collected.
        current_i: u32,
    },
    /// EA → ITS-S: the batch.
    ButterflyAtDownloadResponse {
        /// The station.
        station: NodeId,
        /// The i-period.
        current_i: u32,
        /// How many tickets are in the batch. Zero when the EA's internal blocklist
        /// refused it, which is the passive revocation of §6.1.6 acting on a download.
        count: u32,
    },

    // --- §6.3, the trust lists -----------------------------------------------------
    /// TLM → CPOC: a signed European Certificate Trust List.
    ///
    /// Injected at the TLM to start the flow and then sent to the CPOC, so the handler
    /// tells the two apart by whether it is the TLM being dispatched — the same guard the
    /// enrolment and authorization flows use for their own first step.
    EctlPublish {
        /// The list's `ctlSequence`.
        sequence: u32,
        /// How many certificate-authority entries it carries.
        entries: u32,
        /// The station this run distributes to.
        station: NodeId,
    },
    /// CPOC → ITS-S: the list, over HTTP or an RSU's single-hop broadcast.
    CtlDistribute {
        /// The station receiving it.
        station: NodeId,
        /// The `ctlSequence`.
        sequence: u32,
        /// How many entries.
        entries: u32,
    },
    /// RCA → CPOC: the CA-only certificate revocation list.
    CaCrlPublish {
        /// How many certificate authorities it revokes.
        entries: u32,
        /// The station this run distributes to.
        station: NodeId,
    },
    /// CPOC → ITS-S: the CA-only CRL.
    CaCrlDistribute {
        /// The station receiving it.
        station: NodeId,
        /// How many entries.
        entries: u32,
    },

    // --- TS 103 759, misbehaviour reporting ----------------------------------------
    /// ITS-S → MA: an `EtsiTs103759Data` report, signed with the AT and encrypted to the
    /// MA.
    MisbehaviourReport {
        /// The reporter.
        station: NodeId,
        /// The station the report is about.
        subject: NodeId,
    },
    /// MA → MA: the optional pre-processing stage of TS 103 759 §4, as a timer on the
    /// authority itself so that its service time is charged before collection.
    ReportPreProcessed {
        /// The reporter.
        station: NodeId,
        /// The subject.
        subject: NodeId,
    },
    /// MA → EA: block this station's enrolment credential.
    ///
    /// The EA's blocklist is never published [TS 102 941 §6.1.6], so there is no list to
    /// distribute and no per-vehicle CRL: the subject keeps transmitting until its ticket
    /// pool runs out.
    BlockEnrolment {
        /// The subject.
        subject: NodeId,
    },
}

/// Hand-written sizes for the built flows.
///
/// Every certificate in every message below is sized by the **real COER encoder** through
/// [`CertificateSizes`], so a change to the certificate profile moves every trust list and
/// every ticket batch with it. What is hand-written is the *framing* around them, because
/// build decision D5 defers the TS 102 941 ASN.1 on a `WITH COMPONENTS` inner-subtyping
/// construct at `EtsiTs102941MessagesItss.asn:105:6` and there is therefore no encoder for
/// `EtsiTs102941Data`. Each such sum names the card parameter it rests on, so
/// `tests/wire_sizes.rs` can walk from a byte count on a link to the plan for pinning it
/// down (invariant I-P8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EtsiSizes {
    certs: CertificateSizes,
    subject_attributes_bytes: u32,
    butterfly_response_bytes: u32,
    ctl_framing_bytes: u32,
    report_payload_bytes: u32,
}

impl EtsiSizes {
    /// The sizes for a deployment, with the framing blocks no clause fixes.
    pub const fn new_with(
        certs: CertificateSizes,
        subject_attributes_bytes: u32,
        butterfly_response_bytes: u32,
        ctl_framing_bytes: u32,
        report_payload_bytes: u32,
    ) -> EtsiSizes {
        EtsiSizes {
            certs,
            subject_attributes_bytes,
            butterfly_response_bytes,
            ctl_framing_bytes,
            report_payload_bytes,
        }
    }

    /// The sizes a whole [`EtsiParams`] implies.
    #[must_use]
    pub fn of(certs: CertificateSizes, params: &EtsiParams) -> EtsiSizes {
        EtsiSizes::new_with(
            certs,
            params.subject_attributes_bytes,
            params.butterfly_response_bytes,
            params.ctl_framing_bytes,
            params.report_payload_bytes,
        )
    }

    /// The sizes for a deployment, with the other two framing blocks defaulted from
    /// [`EtsiParams`].
    ///
    /// Kept because it is the constructor the standard-variant flows were written against;
    /// [`EtsiSizes::new_with`] is the one a deployment building the butterfly or reporting
    /// flows wants.
    pub fn new(certs: CertificateSizes, subject_attributes_bytes: u32) -> EtsiSizes {
        let d = EtsiParams::default();
        EtsiSizes::new_with(
            certs,
            subject_attributes_bytes,
            d.butterfly_response_bytes,
            d.ctl_framing_bytes,
            d.report_payload_bytes,
        )
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

    /// `ButterflyAuthorizationRequest`: a caterpillar signing pair and a caterpillar
    /// encryption pair, with the proof-of-possession inner signature and the outer one
    /// [TS 102 941 §6.2.3.5, Fig. 23].
    ///
    /// Two public points rather than the standard variant's two, and *no* per-ticket
    /// material: that is the whole economy of the butterfly construction, and it is why
    /// this one request replaces the standard variant's one request per ticket.
    pub const fn butterfly_authorization_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + HASHED_ID8_BYTES
                + 2 * EC_POINT_COMPRESSED_BYTES
                + self.subject_attributes_bytes
                + 2 * ECDSA_P256_SIG_COER_BYTES
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_subject_attributes_bytes",
        )
    }

    /// `ButterflyAuthorizationResponse`: `currentI`, `requestHash` and `nextDlTime`.
    pub const fn butterfly_acknowledgement(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + self.butterfly_response_bytes
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_butterfly_response_bytes",
        )
    }

    /// One `ButterflyCertRequest`, EA → AA: a cocoon public key with its key tag.
    ///
    /// Per cocoon key, because §6.2.3.5 has the EA send "multiple `ButterflyCertRequest`s
    /// to the AA" — one per expanded key — and a single aggregate would hide the per-ticket
    /// byte cost the whole comparison against the standard variant is about.
    pub const fn butterfly_cert_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + EC_POINT_COMPRESSED_BYTES
                + AES128_KEY_BYTES
                + self.subject_attributes_bytes
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_subject_attributes_bytes",
        )
    }

    /// One `ButterflyCertResponse`, AA → EA: one authorization ticket, encrypted.
    pub const fn butterfly_cert_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + self.certs.pseudonym.bytes()
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "envelope(digest) + authorization ticket + ECIES wrapper",
            STRUCTURE,
        )
    }

    /// `ButterflyAtDownloadRequest`: the station asks for its batch.
    pub const fn butterfly_at_download_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + HASHED_ID8_BYTES
                + self.butterfly_response_bytes
                + ECDSA_P256_SIG_COER_BYTES
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_butterfly_response_bytes",
        )
    }

    /// `ButterflyAtDownloadResponse`: `count` tickets, each encrypted to the station.
    pub const fn butterfly_at_download_response(&self, count: u32) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + count * (self.certs.pseudonym.bytes() + ECIES_P256_ENCRYPTED_KEY_BYTES),
            "envelope(certificate) + EA certificate + count x (authorization ticket +              ECIES wrapper)",
            STRUCTURE,
        )
    }

    /// The European Certificate Trust List: `entries` CA certificates with their link
    /// certificates, signed by the Trust List Manager [TS 102 941 §6.3.1].
    ///
    /// Each entry is a real authority certificate from the COER encoder plus one
    /// `HashedId8` of link-certificate reference, so the list's size moves with the
    /// certificate profile rather than with a constant.
    pub const fn ectl(&self, entries: u32) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + self.ctl_framing_bytes
                + entries * (self.certs.authority.bytes() + HASHED_ID8_BYTES),
            "etsi_ctl_framing_bytes",
        )
    }

    /// The Root CA's CA-only CRL: `entries` revoked certificate authorities, by
    /// `HashedId8` with an expiry [TS 102 941 §6.3.5].
    ///
    /// There is no per-vehicle CRL to size: §6.1.4 NOTE 4 states that "revocation of
    /// authorization tickets is not possible as passive revocation is preferred".
    pub const fn ca_crl(&self, entries: u32) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + self.ctl_framing_bytes
                + entries * (HASHED_ID8_BYTES + TIME32_BYTES),
            "etsi_ctl_framing_bytes",
        )
    }

    /// A TS 103 759 misbehaviour report: the payload, signed with the reporter's ticket
    /// and encrypted to the Misbehaviour Authority.
    ///
    /// The payload rests on a card parameter and not on a clause, for the same reason the
    /// SCMS plug-in's does: TS 103 759 §4.2.4 requires the evidence to include "the
    /// original received messages with their AT", and how many messages a detector cites
    /// is a policy rather than a size.
    pub const fn misbehaviour_report(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.pseudonym.bytes()
                + self.report_payload_bytes
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "etsi_report_payload_bytes",
        )
    }

    /// MA → EA: block one station's enrolment credential.
    ///
    /// Small, and that is the point of passive revocation: the artefact that revokes a
    /// vehicle is one identifier on an internal list, not a list broadcast to a fleet.
    pub const fn block_enrolment(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + HASHED_ID8_BYTES
                + TIME32_BYTES
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "envelope(certificate) + MA certificate + subject HashedId8 + effective time              + ECIES wrapper",
            STRUCTURE,
        )
    }

    /// `count` cocoon keys in one EA -> AA batch.
    pub const fn butterfly_cert_request_batch(&self, count: u32) -> WireSize {
        WireSize::parameter(
            self.butterfly_cert_request().bytes() * count,
            "etsi_subject_attributes_bytes",
        )
    }

    /// `count` certified tickets in one AA -> EA batch.
    pub const fn butterfly_cert_response_batch(&self, count: u32) -> WireSize {
        WireSize::derived(
            self.butterfly_cert_response().bytes() * count,
            "count x (envelope(digest) + authorization ticket + ECIES wrapper)",
            STRUCTURE,
        )
    }

    /// Every message kind, for the conformance test.
    ///
    /// The two `count`-dependent sizes are listed at one element, which is the shape the
    /// provenance check needs; the flows themselves size them at the batch they actually
    /// carry.
    pub fn table(&self) -> Vec<(&'static str, WireSize)> {
        vec![
            ("etsi-enrolment-request", self.enrolment_request()),
            ("etsi-enrolment-response", self.enrolment_response()),
            ("etsi-authorization-request", self.authorization_request()),
            ("etsi-validation-request", self.validation_request()),
            ("etsi-validation-response", self.validation_response()),
            ("etsi-authorization-response", self.authorization_response()),
            (
                "etsi-butterfly-authorization-request",
                self.butterfly_authorization_request(),
            ),
            (
                "etsi-butterfly-acknowledgement",
                self.butterfly_acknowledgement(),
            ),
            ("etsi-butterfly-cert-request", self.butterfly_cert_request()),
            (
                "etsi-butterfly-cert-response",
                self.butterfly_cert_response(),
            ),
            (
                "etsi-butterfly-at-download-request",
                self.butterfly_at_download_request(),
            ),
            (
                "etsi-butterfly-at-download-response",
                self.butterfly_at_download_response(1),
            ),
            ("etsi-ectl-publish", self.ectl(1)),
            ("etsi-ctl-distribute", self.ectl(1)),
            ("etsi-ca-crl-publish", self.ca_crl(1)),
            ("etsi-ca-crl-distribute", self.ca_crl(1)),
            ("etsi-misbehaviour-report", self.misbehaviour_report()),
            ("etsi-block-enrolment", self.block_enrolment()),
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

/// The flows the butterfly, trust-list and reporting paths run, and the stages they emit.
///
/// Declared beside [`FLOWS`] rather than in it so that a reader can see at a glance which
/// flows the skeleton grew and which two it started with; [`all_flows`] is the
/// concatenation, and invariant I-P4's test walks that.
pub const DEFERRED_FLOWS: &[FlowSpec] = &[
    FlowSpec {
        id: FlowId::EtsiButterflyAuthorization,
        participants: &["ITS-S", "EA", "AA"],
        stages: &[
            StageId::Requested,
            StageId::Acknowledged,
            StageId::Expanded,
            StageId::Certified,
            StageId::BatchReady,
        ],
    },
    FlowSpec {
        id: FlowId::EtsiAtDownload,
        participants: &["ITS-S", "EA"],
        stages: &[StageId::Requested, StageId::Downloaded, StageId::Installed],
    },
    FlowSpec {
        id: FlowId::EtsiTrustList,
        participants: &["TLM", "CPOC", "ITS-S"],
        stages: &[
            StageId::Issued,
            StageId::Published,
            StageId::Downloaded,
            StageId::Processed,
        ],
    },
    FlowSpec {
        id: FlowId::EtsiCaCrl,
        participants: &["RCA", "CPOC", "ITS-S"],
        stages: &[
            StageId::Issued,
            StageId::Published,
            StageId::Downloaded,
            StageId::Processed,
            StageId::Enforced,
        ],
    },
    FlowSpec {
        id: FlowId::EtsiMisbehaviourReport,
        participants: &["ITS-S", "MA", "EA"],
        stages: &[
            StageId::Detect,
            StageId::ReportSent,
            StageId::ReportReceived,
            StageId::Decision,
            StageId::Blocklisted,
        ],
    },
];

/// Every flow this plug-in runs, the two original ones first.
#[must_use]
pub fn all_flows() -> Vec<FlowSpec> {
    FLOWS.iter().chain(DEFERRED_FLOWS).cloned().collect()
}

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

/// The one active list this protocol has: the Root CA's CA-only CRL.
///
/// 05-protocols.md §4.3 binds ETSI revocation as "`Passive` for vehicles (EA blocklist);
/// `Active` CA-CRL only (series: CA certificates)", and this is the second half. An entry
/// is a `HashedId8` with an expiry — 12 bytes against the SCMS's ≈ 40 B of linkage seeds —
/// and processing one is a store insertion rather than a hash chain, which is the whole
/// cost difference between revoking authorities and revoking vehicles.
///
/// The cadence is the trust-list one: EUCP §2.2 puts trust-material updates at "≤ 3
/// months" with stations obliged to update within a week, and gives no separate CA-CRL
/// period. It is a card parameter with that plan attached.
pub const CA_CRL_REVOCATION: ActiveRevocation = ActiveRevocation {
    entry: WireSize::derived(
        HASHED_ID8_BYTES + TIME32_BYTES,
        "HashedId8 + expiry Time32",
        STRUCTURE,
    ),
    // [BRECHT §VI-G]'s series 1 is pseudonym certificates; a CA-only list is not that
    // series, and TS 102 941 has no series numbering of its own, so this is the
    // certificate-authority series and the card says the number is this crate's.
    series: 2,
    cadence: Duration::from_secs(90 * 24 * 60 * 60),
    stages: &[
        StageId::Issued,
        StageId::Published,
        StageId::Downloaded,
        StageId::Processed,
        StageId::Enforced,
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

    /// Revocation: passive for vehicles, active for certificate authorities only.
    ///
    /// This used to answer `Passive` alone, which was true of the skeleton and not of the
    /// protocol: TS 102 941 §6.3.5 has the Root CA sign a CA-only CRL, and 05-protocols.md
    /// §4.3 binds the pair. The vehicle half is still passive — §6.1.4 NOTE 4 is explicit
    /// that "revocation of authorization tickets is not possible as passive revocation is
    /// preferred" — so a study comparing revocation latency between the two protocols is
    /// comparing an SCMS list of vehicles against an ETSI list of authorities plus a
    /// starvation horizon, and [`EtsiTs102941::passive_eviction_bound`] is that horizon.
    pub const fn revocation(&self) -> RevocationMechanism {
        RevocationMechanism::Both(CA_CRL_REVOCATION, PASSIVE_REVOCATION)
    }

    /// The flows this plug-in runs, including the ones build decision D5 deferred.
    ///
    /// [`EtsiTs102941::flows`] returns only the two the skeleton started with, because it
    /// is `const` and returns a `'static` slice; this is the whole set.
    #[must_use]
    pub fn all_flows(&self) -> Vec<FlowSpec> {
        all_flows()
    }

    /// The flows that were declared-and-not-built and now are: the butterfly variant, the
    /// ticket download, the two trust lists and the reporting path.
    pub const fn deferred_flows(&self) -> &'static [FlowSpec] {
        DEFERRED_FLOWS
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
    /// Ticket batches the AA has certified and the EA holds, by station: the i-period and
    /// how many tickets are waiting for a `ButterflyAtDownloadRequest`.
    ///
    /// The EA and not the station, because §6.2.3.5 has the station come back for its
    /// batch at `nextDlTime`: the repository is the authority's, and the download is a
    /// separate flow with its own latency for exactly that reason.
    pub pending_batches: BTreeMap<NodeId, (u32, u32)>,
    /// The i-period the Enrolment Authority is expanding caterpillar keys for.
    ///
    /// TS 102 941 §6.2.3.5 has the EA return `currentI` in its acknowledgement, and a
    /// scenario advances this between batches. Zero is the honest start: nothing in the
    /// clause fixes an epoch, and the SCMS plug-in's i-period is the protocol's own week
    /// counter rather than something this skeleton can borrow.
    pub current_i: u32,
    /// The `ctlSequence` of the last European Certificate Trust List the TLM signed.
    pub ctl_sequence: u32,
    /// The `ctlSequence` each station has installed.
    pub installed_ctl: BTreeMap<NodeId, u32>,
    /// How many CA-CRL entries each station is enforcing.
    pub installed_ca_crl: BTreeMap<NodeId, u32>,
    /// How many reports the Misbehaviour Authority has received about each subject.
    pub reports: BTreeMap<NodeId, u32>,
    /// How many reports went through the optional pre-processing stage.
    pub pre_processed: u32,
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
        let sizes = EtsiSizes::of(CertificateSizes::measured()?, &params);
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
            pending_batches: BTreeMap::new(),
            current_i: 0,
            ctl_sequence: 0,
            installed_ctl: BTreeMap::new(),
            installed_ca_crl: BTreeMap::new(),
            reports: BTreeMap::new(),
            pre_processed: 0,
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
        let (ea, aa, cpoc, ma) = (self.nodes.ea, self.nodes.aa, self.nodes.cpoc, self.nodes.ma);
        {
            let net = self.kernel.net_mut();
            net.connect(station, ea, link);
            net.connect(station, aa, link);
            // TS 102 941 §6.2.2 lists the connectivity options a station reaches the PKI
            // through — ITS-G5 via an RSU, WLAN, cellular, an EV charger, OBD at a garage.
            // Whichever it is, the trust-list download and the misbehaviour-report upload
            // are two more paths, and invariant I-P1 refuses a message over a link that
            // does not exist rather than delivering it for free.
            net.connect(station, cpoc, link);
            net.connect(station, ma, link);
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

    /// Starts the butterfly authorization flow of TS 102 941 §6.2.3.5.
    ///
    /// One request in place of one per ticket: the station sends a single caterpillar pair,
    /// the EA expands it into [`EtsiParams::butterfly_batch`] cocoon keys and hands them to
    /// the AA, and the certified batch waits at the EA until the station comes back for it
    /// with [`EtsiRun::download_ats`]. That asymmetry — one uplink request, a batch
    /// downlink — is the whole reason the variant exists, and it is what makes
    /// `tests/etsi.rs`'s byte comparison against the standard variant meaningful.
    pub fn authorize_butterfly(&mut self, station: NodeId) -> FlowRun {
        let run = self.new_run();
        self.inject(
            station,
            Ts102941Msg::ButterflyAuthorizationRequest { station },
            FlowId::EtsiButterflyAuthorization,
            run,
        );
        run
    }

    /// Starts the authorization-ticket batch download of TS 102 941 §6.2.3.5.
    ///
    /// The EA authorises the download against its internal blocklist, which is where
    /// passive revocation bites on this variant: a blocklisted station's request is
    /// answered with an empty batch and the batch it had already been certified is
    /// discarded.
    pub fn download_ats(&mut self, station: NodeId, current_i: u32) -> FlowRun {
        let run = self.new_run();
        self.inject(
            station,
            Ts102941Msg::ButterflyAtDownloadRequest { station, current_i },
            FlowId::EtsiAtDownload,
            run,
        );
        run
    }

    /// Signs and distributes a European Certificate Trust List to one station
    /// (TS 102 941 §6.3.1-6.3.3).
    ///
    /// One station per run, exactly as the SCMS plug-in's `distribute_crl` is one device
    /// per run: the flow's declared stages are per-node from `downloaded` onwards, and a
    /// run that distributed to a fleet would stamp `downloaded` and `processed` once per
    /// station and no longer match its own declaration.
    pub fn publish_ectl(&mut self, station: NodeId) -> FlowRun {
        let run = self.new_run();
        self.ctl_sequence = self.ctl_sequence.saturating_add(1);
        let (sequence, entries) = (self.ctl_sequence, self.params.ctl_entries);
        let tlm = self.nodes.tlm;
        self.inject(
            tlm,
            Ts102941Msg::EctlPublish {
                sequence,
                entries,
                station,
            },
            FlowId::EtsiTrustList,
            run,
        );
        run
    }

    /// Signs and distributes the Root CA's CA-only CRL to one station
    /// (TS 102 941 §6.3.5).
    ///
    /// The *only* active revocation list in this protocol. §6.1.4 NOTE 4 is explicit that
    /// "revocation of authorization tickets is not possible as passive revocation is
    /// preferred", so there is no per-vehicle list here and the comparison against the
    /// SCMS's linked CRL is a comparison of one list of certificate authorities against
    /// one list of vehicles.
    pub fn publish_ca_crl(&mut self, station: NodeId) -> FlowRun {
        let run = self.new_run();
        let (entries, rca) = (self.params.ca_crl_entries, self.nodes.rca);
        self.inject(
            rca,
            Ts102941Msg::CaCrlPublish { entries, station },
            FlowId::EtsiCaCrl,
            run,
        );
        run
    }

    /// Starts a TS 103 759 misbehaviour report.
    ///
    /// The *decision* is taken on the first report, and that is a scope boundary rather
    /// than a model of the authority: report clustering, windowed correlation and the
    /// investigation are the `MaPipeline` family's (03-interfaces.md §9, `v2xw-threat`),
    /// and duplicating a threshold here would give a run two answers to "was this device
    /// reported enough". What this flow models is the *path* — sign with the ticket,
    /// encrypt to the authority, optionally pre-process, decide, block the enrolment
    /// credential — and the latency of each hop.
    pub fn report(&mut self, reporter: NodeId, subject: NodeId) -> FlowRun {
        let run = self.new_run();
        self.inject(
            reporter,
            Ts102941Msg::MisbehaviourReport {
                station: reporter,
                subject,
            },
            FlowId::EtsiMisbehaviourReport,
            run,
        );
        run
    }

    /// How many tickets `station` holds.
    #[must_use]
    pub fn tickets_of(&self, station: NodeId) -> u32 {
        self.tickets.get(&station).copied().unwrap_or(0)
    }

    /// The `ctlSequence` `station` has installed, if any.
    #[must_use]
    pub fn installed_ctl_of(&self, station: NodeId) -> Option<u32> {
        self.installed_ctl.get(&station).copied()
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

            // --- §6.2.3.5, the butterfly variant ---------------------------------
            Ts102941Msg::ButterflyAuthorizationRequest { station } if to == station => {
                // One caterpillar signing pair and one caterpillar encryption pair, and
                // two signatures: the proof of possession and the outer one. Not one key
                // pair per ticket — that is the whole economy of the construction.
                out.charge(PrimitiveId::ECDSA_P256_SHA256, PrimitiveOpKind::KeyGen, 2);
                sign(out, 2);
                out.stage_at(StageId::Requested, station, None, flow, run);
                out.send(
                    n.ea,
                    Ts102941Msg::ButterflyAuthorizationRequest { station },
                    "etsi-butterfly-authorization-request",
                    self.sizes.butterfly_authorization_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ButterflyAuthorizationRequest { station } => {
                verify(out, 2);
                sign(out, 1);
                let granted =
                    self.enrolled.contains(&station) && !self.blocklist.contains(&station);
                let current_i = self.current_i;
                if !granted {
                    self.refused += 1;
                }
                if granted {
                    out.stage_at(StageId::Acknowledged, to, None, flow, run);
                }
                out.send(
                    station,
                    Ts102941Msg::ButterflyAcknowledgement {
                        station,
                        current_i,
                        granted,
                    },
                    "etsi-butterfly-acknowledgement",
                    self.sizes.butterfly_acknowledgement(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
                if granted {
                    // The expansion: one elliptic-curve point derivation per cocoon key.
                    // Charged as a key-generation operation because that is the primitive
                    // the cost table has for a scalar multiplication on P-256, and the
                    // count is what a study of the variant's backend cost is about.
                    let count = self.params.butterfly_batch.max(1);
                    out.charge(
                        PrimitiveId::ECDSA_P256_SHA256,
                        PrimitiveOpKind::KeyGen,
                        count,
                    );
                    out.stage_at(StageId::Expanded, to, None, flow, run);
                    out.send(
                        n.aa,
                        Ts102941Msg::ButterflyCertRequest {
                            station,
                            current_i,
                            count,
                        },
                        "etsi-butterfly-cert-request",
                        self.sizes.butterfly_cert_request_batch(count),
                        Transport::BackendNet,
                        flow,
                        run,
                    );
                }
            }
            Ts102941Msg::ButterflyAcknowledgement { .. } => {
                // The station learns `currentI`, `requestHash` and `nextDlTime` and comes
                // back for its batch; nothing is installed here, which is why the download
                // is its own flow.
                verify(out, 1);
            }
            Ts102941Msg::ButterflyCertRequest {
                station,
                current_i,
                count,
            } => {
                verify(out, 1);
                // One signature per certificate: the AA certifies each cocoon key. This is
                // the cost the butterfly variant does *not* save — it saves round trips and
                // uplink bytes, not signatures.
                sign(out, count);
                out.stage_at(StageId::Certified, to, None, flow, run);
                out.send(
                    n.ea,
                    Ts102941Msg::ButterflyCertResponse {
                        station,
                        current_i,
                        count,
                    },
                    "etsi-butterfly-cert-response",
                    self.sizes.butterfly_cert_response_batch(count),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ButterflyCertResponse {
                station,
                current_i,
                count,
            } => {
                verify(out, 1);
                self.pending_batches.insert(station, (current_i, count));
                let bytes = self.sizes.butterfly_at_download_response(count).bytes();
                out.stage_at(StageId::BatchReady, to, Some(bytes), flow, run);
            }
            Ts102941Msg::ButterflyAtDownloadRequest { station, current_i } if to == station => {
                sign(out, 1);
                out.stage_at(StageId::Requested, station, None, flow, run);
                out.send(
                    n.ea,
                    Ts102941Msg::ButterflyAtDownloadRequest { station, current_i },
                    "etsi-butterfly-at-download-request",
                    self.sizes.butterfly_at_download_request(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ButterflyAtDownloadRequest { station, current_i } => {
                verify(out, 1);
                sign(out, 1);
                // §6.2.3.5 authorises the download "by the EA's internal blocklist or an
                // OAuth token". This is the blocklist branch, and it is where passive
                // revocation bites on this variant: the batch was already certified, and
                // the station never gets it.
                let blocked = self.blocklist.contains(&station);
                let pending = self.pending_batches.remove(&station);
                let count = match (blocked, pending) {
                    (false, Some((i, c))) if i == current_i => c,
                    (false, Some((i, c))) => {
                        // A request for a period the EA has no batch for: the batch stays
                        // and the answer is empty, rather than handing over the wrong one.
                        self.pending_batches.insert(station, (i, c));
                        0
                    }
                    _ => {
                        if blocked {
                            self.refused += 1;
                        }
                        0
                    }
                };
                out.send(
                    station,
                    Ts102941Msg::ButterflyAtDownloadResponse {
                        station,
                        current_i,
                        count,
                    },
                    "etsi-butterfly-at-download-response",
                    self.sizes.butterfly_at_download_response(count),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::ButterflyAtDownloadResponse { station, count, .. } => {
                verify(out, 1);
                if count > 0 {
                    let bytes = self.sizes.butterfly_at_download_response(count).bytes();
                    out.stage_at(StageId::Downloaded, station, Some(bytes), flow, run);
                    *self.tickets.entry(station).or_insert(0) += count;
                    out.stage_at(StageId::Installed, station, None, flow, run);
                }
            }

            // --- §6.3, the trust lists -------------------------------------------
            Ts102941Msg::EctlPublish {
                sequence,
                entries,
                station,
            } if to == n.tlm => {
                sign(out, 1);
                let bytes = self.sizes.ectl(entries).bytes();
                out.stage_at(StageId::Issued, to, Some(bytes), flow, run);
                out.send(
                    n.cpoc,
                    Ts102941Msg::EctlPublish {
                        sequence,
                        entries,
                        station,
                    },
                    "etsi-ectl-publish",
                    self.sizes.ectl(entries),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            Ts102941Msg::EctlPublish {
                sequence,
                entries,
                station,
            } => {
                verify(out, 1);
                let bytes = self.sizes.ectl(entries).bytes();
                out.stage_at(StageId::Published, to, Some(bytes), flow, run);
                out.send(
                    station,
                    Ts102941Msg::CtlDistribute {
                        station,
                        sequence,
                        entries,
                    },
                    "etsi-ctl-distribute",
                    self.sizes.ectl(entries),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::CtlDistribute {
                station,
                sequence,
                entries,
            } => {
                // The TLM's signature, plus one verification per certificate-authority
                // entry: a station that installs a trust list checks what it is trusting.
                verify(out, 1 + entries);
                let bytes = self.sizes.ectl(entries).bytes();
                out.stage_at(StageId::Downloaded, station, Some(bytes), flow, run);
                self.installed_ctl.insert(station, sequence);
                out.stage_at(StageId::Processed, station, None, flow, run);
            }
            Ts102941Msg::CaCrlPublish { entries, station } if to == n.rca => {
                sign(out, 1);
                let bytes = self.sizes.ca_crl(entries).bytes();
                out.stage_at(StageId::Issued, to, Some(bytes), flow, run);
                out.send(
                    n.cpoc,
                    Ts102941Msg::CaCrlPublish { entries, station },
                    "etsi-ca-crl-publish",
                    self.sizes.ca_crl(entries),
                    Transport::BackendNet,
                    flow,
                    run,
                );
            }
            Ts102941Msg::CaCrlPublish { entries, station } => {
                verify(out, 1);
                let bytes = self.sizes.ca_crl(entries).bytes();
                out.stage_at(StageId::Published, to, Some(bytes), flow, run);
                out.send(
                    station,
                    Ts102941Msg::CaCrlDistribute { station, entries },
                    "etsi-ca-crl-distribute",
                    self.sizes.ca_crl(entries),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::CaCrlDistribute { station, entries } => {
                verify(out, 1);
                let bytes = self.sizes.ca_crl(entries).bytes();
                out.stage_at(StageId::Downloaded, station, Some(bytes), flow, run);
                // No hash chain to walk and no linkage values to expand: a CA-CRL entry is
                // a `HashedId8` with an expiry, so processing it is a store insertion and
                // the per-entry cost the SCMS's linked CRL pays does not exist here. That
                // asymmetry is one of the things a comparison between the two protocols is
                // for, and it is why this arm charges no expansion.
                self.installed_ca_crl.insert(station, entries);
                out.stage_at(StageId::Processed, station, None, flow, run);
                out.stage_at(StageId::Enforced, station, None, flow, run);
            }

            // --- TS 103 759, misbehaviour reporting -------------------------------
            Ts102941Msg::MisbehaviourReport { station, subject } if to == station => {
                sign(out, 1);
                out.stage_at(StageId::Detect, station, None, flow, run);
                out.stage_at(StageId::ReportSent, station, None, flow, run);
                out.send(
                    n.ma,
                    Ts102941Msg::MisbehaviourReport { station, subject },
                    "etsi-misbehaviour-report",
                    self.sizes.misbehaviour_report(),
                    Transport::CellularUu,
                    flow,
                    run,
                );
            }
            Ts102941Msg::MisbehaviourReport { station, subject } => {
                // The report is decrypted and its outer signature checked. §4.2.4 requires
                // the evidence to carry "the original received messages with their AT so
                // the MA can re-verify", and that re-verification is the pre-processing
                // step below — charged, never free.
                verify(out, 1);
                if self.params.report_pre_processing {
                    // A zero-delay self-message, so the pre-processing gets its own pass
                    // through the authority's service queue and is charged as a second
                    // step. Zero rather than an invented delay: TS 103 759 §4 makes
                    // pre-processing optional and gives it no duration.
                    out.start_timer(
                        Duration::ZERO,
                        Ts102941Msg::ReportPreProcessed { station, subject },
                        flow,
                        run,
                    );
                } else {
                    out.stage_at(StageId::ReportReceived, to, None, flow, run);
                    self.decide(out, subject, to, n.ea, flow, run);
                }
            }
            Ts102941Msg::ReportPreProcessed { station, subject } => {
                let _ = station;
                // Re-verifying the cited messages: one verification per message the report
                // carries, and the count is the report payload's to know. One is charged
                // here, because the payload size is a card parameter and a per-message
                // count derived from it would be a second invented number.
                verify(out, 1);
                self.pre_processed += 1;
                out.stage_at(StageId::ReportReceived, to, None, flow, run);
                self.decide(out, subject, to, n.ea, flow, run);
            }
            Ts102941Msg::BlockEnrolment { subject } => {
                verify(out, 1);
                self.blocklist.insert(subject);
                out.stage_at(StageId::Blocklisted, to, None, flow, run);
            }
        }
    }

    /// The Misbehaviour Authority's decision, and the message that carries it.
    ///
    /// Split out because both the pre-processed and the un-pre-processed paths reach it and
    /// a second copy is how the two would come to disagree.
    fn decide(
        &mut self,
        out: &mut Outbox<Ts102941Msg>,
        subject: NodeId,
        ma: NodeId,
        ea: NodeId,
        flow: FlowId,
        run: FlowRun,
    ) {
        use v2xw_sec::primitive::{PrimitiveId, PrimitiveOpKind};
        *self.reports.entry(subject).or_insert(0) += 1;
        out.charge(PrimitiveId::ECDSA_P256_SHA256, PrimitiveOpKind::Sign, 1);
        out.stage_at(StageId::Decision, ma, None, flow, run);
        out.send(
            ea,
            Ts102941Msg::BlockEnrolment { subject },
            "etsi-block-enrolment",
            self.sizes.block_enrolment(),
            Transport::BackendNet,
            flow,
            run,
        );
    }
}

fn card(p: &EtsiParams) -> ModelCard {
    let mut card = ModelCard::new(
        ETSI_TS102941_ID,
        Family::Protocol,
        "0.2.0",
        "ETSI TS 102 941 ITS PKI: enrolment, standard authorization, butterfly          authorization with a separate ticket download, ECTL and CA-CRL distribution, and          the TS 103 759 reporting path, as flows over queued Enrolment, Authorization,          Trust-List and Misbehaviour Authorities. Message structures are hand-written and          every certificate in them is sized by the real COER encoder, because build          decision D5 defers the ETSI PKI ASN.1 on an inner-subtyping construct.",
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
    card.parameters.extend([
        Parameter::new(
            "ctl_validity_days",
            "d",
            serde_json::json!(p.ctl_validity.as_nanos() / (86_400 * 1_000_000_000)),
            Source::new(
                SourceKind::Standard,
                "EU C-ITS Certificate Policy 1.1 §2.2: trust-material update cadence                  <= 3 months, with stations obliged to update within 1 week (via                  05-protocols.md §2.6)",
            ),
        ),
        {
            let mut param = Parameter::new(
                "etsi_butterfly_response_bytes",
                "B",
                serde_json::json!(p.butterfly_response_bytes),
                Source::todo_calibrate(
                    "no clause this build can read fixes the currentI / requestHash /                      nextDlTime block of a ButterflyAuthorizationResponse",
                ),
            );
            param.calibration = Some(
                "Size it from the real encoder once build decision D5's inner-subtyping                  failure at EtsiTs102941MessagesItss.asn:105:6 is resolved. Until then it                  is a scenario parameter and the acknowledgement and the download request                  are both marked as resting on it."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "etsi_ctl_framing_bytes",
                "B",
                serde_json::json!(p.ctl_framing_bytes),
                Source::todo_calibrate(
                    "no clause this build can read fixes the trust-list framing:                      nextUpdate, ctlSequence and the list container",
                ),
            );
            param.calibration = Some(
                "As etsi_butterfly_response_bytes: the real encoder answers it, and D5                  defers the real encoder. The per-entry part does not rest on this — every                  certificate in an ECTL is sized by the COER encoder — so the parameter's                  share of a trust list shrinks as the list grows, which is the right way                  round for a number nobody has pinned down."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "etsi_report_payload_bytes",
                "B",
                serde_json::json!(p.report_payload_bytes),
                Source::todo_calibrate(
                    "TS 103 759 §4.2.4 requires the evidence to carry the original                      received messages with their AT and does not fix how many",
                ),
            );
            param.calibration = Some(
                "How many messages a detector cites is a policy, not a size: measure it                  from the detector suite's own evidence buffers (07-threats-and-detection.md                  §5) and multiply by a secured CAM or BSM. The SCMS plug-in carries the                  same number under report_payload_bytes with the same plan, and the two                  should be calibrated together."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "butterfly_batch",
                "-",
                serde_json::json!(p.butterfly_batch),
                Source::todo_calibrate(
                    "TS 102 941 §6.2.3.5 gives the butterfly expansion its shape and no                      ticket count",
                ),
            );
            param.calibration = Some(
                "20 is the C2C-CC basic system profile's parallel-ticket count                  [TR 103 415 Table A.2]; the EU Certificate Policy caps the *concurrent*                  pool at 100 [EUCP §7.2.1]. Neither is a batch size. Set it from the                  deployment being modelled, and note that it scales the AA's signature                  count and the download's bytes linearly — which is what a comparison                  against the standard variant measures."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "at_download_batch",
                "-",
                serde_json::json!(p.at_download_batch),
                Source::todo_calibrate("as butterfly_batch: no clause fixes a batch size"),
            );
            param.calibration = Some(
                "Defaulted to butterfly_batch so that one download collects one                  expansion, which is the simplest reading of §6.2.3.5's 'downloads                  batches'. A deployment that splits a batch across downloads sets it                  smaller and pays the request bytes again."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "ctl_entries",
                "-",
                serde_json::json!(p.ctl_entries),
                Source::todo_calibrate(
                    "how many certificate authorities the European Certificate Trust List                      holds is a deployment fact, not a clause",
                ),
            );
            param.calibration = Some(
                "Read the published ECTL from the CPOC and count its entries. It is the                  dominant term in a trust list's size — every entry is a real CA                  certificate from the COER encoder — so a study of trust-list bytes over                  an RSU must set it."
                    .into(),
            );
            param
        },
        {
            let mut param = Parameter::new(
                "ca_crl_entries",
                "-",
                serde_json::json!(p.ca_crl_entries),
                Source::todo_calibrate(
                    "how many certificate authorities are revoked is a deployment fact",
                ),
            );
            param.calibration = Some(
                "One by default, which is the scenario a CA-CRL study is about: a single                  compromised authority. Read the published CA-CRL for a real figure; note                  that it is bounded by ctl_entries, which is why it is small where an SCMS                  CRL is not."
                    .into(),
            );
            param
        },
        Parameter::new(
            "report_pre_processing",
            "-",
            serde_json::json!(p.report_pre_processing),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 759 §4: pre-processing before collection is optional, which                  is why this is a flag and not a stage every run emits",
            ),
        ),
    ]);
    card.assumptions = vec![
        "There is no per-vehicle CRL: TS 102 941 §6.1.4 NOTE 4 states that revocation of          authorization tickets is not possible because passive revocation is preferred.          The CA-only CRL of §6.3.5 is the one active list, and it revokes authorities."
            .into(),
        "The Misbehaviour Authority decides on the first report. Report clustering,          windowed correlation and the investigation are the MaPipeline family's          (03-interfaces.md §9), and a second threshold here would give a run two answers          to 'was this device reported enough'."
            .into(),
        "Pre-processing is charged as a second pass through the authority's service queue          with a zero delay: TS 103 759 §4 makes it optional and gives it no duration, so          the work is charged and no latency is invented."
            .into(),
        "A trust-list run distributes to one station, as the SCMS plug-in's CRL          distribution does: the stages from `downloaded` onwards are per-node, and a run          that fanned out to a fleet would no longer match its own declaration."
            .into(),
    ];
    card.limitations = vec![
        "Message *framing* is hand-written sums of cited field sizes, not encoder output,          because build decision D5 defers the ETSI PKI ASN.1 on an inner subtyping          construct at EtsiTs102941MessagesItss.asn:105:6. Every certificate inside those          messages is sized by the real COER encoder, so the framing is the modelled part          and it is the smaller part of every message that carries a certificate."
            .into(),
        "The RSU delta-CTL broadcast path of Annex D.3 — single hop, no segmentation,          stations re-broadcast unmodified — is not a flow here: the air interface is the          node and radio crates', and `v2xw-node`'s roadside runtime carries the role. What          this crate models is the CPOC-to-station path."
            .into(),
        "The manufacturer SOC interface and the Distribution Centre are declared as roles          and have no flows: TS 102 941 §6.2.2's canonical-key bootstrap happens before          enrolment and the DC is a cache in front of the CPOC."
            .into(),
        "Re-enrolment reuses the enrolment flow rather than modelling the          'outer signature with the current EC' variant of §6.2.3.2, which changes which          key signs and not the shape of the exchange."
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
    card.validation.tests = vec![
        "flows::etsi_flows_emit_their_declared_stages_in_order".to_string(),
        "etsi::the_butterfly_variant_costs_one_uplink_request_instead_of_twenty".to_string(),
        "etsi::a_blocklisted_station_cannot_download_a_batch_it_was_already_certified".to_string(),
        "etsi::a_trust_list_grows_with_the_certificates_in_it".to_string(),
        "etsi::a_report_blocks_the_subjects_enrolment_credential".to_string(),
        "wire_sizes::every_etsi_message_size_has_a_provenance_the_card_backs".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}
