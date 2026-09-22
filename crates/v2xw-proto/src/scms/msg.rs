//! The CAMP SCMS messages and their sizes.
//!
//! Two things live here because they must not drift apart: what crosses a link, and how
//! many bytes it is. Every size is built from [`crate::sizes`]'s components, so a message
//! that grows a field grows on the wire, and the certificate component is whatever the
//! real COER encoder produced for this certificate profile.

use p256::Scalar;
use v2xw_core::ids::NodeId;
use v2xw_sec::butterfly::{Caterpillar, ExpansionKey};
use v2xw_sec::ec::Point;
use v2xw_sec::linkage::{LaId, LinkageSeed, LinkageValue, PreLinkageValue};

use crate::sizes::{
    AES128_KEY_BYTES, CRL_LINKAGE_ENTRY_BYTES, CertificateSizes, EC_POINT_COMPRESSED_BYTES,
    ECIES_P256_ENCRYPTED_KEY_BYTES, ENVELOPE_OVERHEAD_CERT_BYTES, ENVELOPE_OVERHEAD_DIGEST_BYTES,
    HASHED_ID8_BYTES, LINKAGE_VALUE_BYTES, SHA256_BYTES, SizeParams, TIME32_BYTES, WireSize,
};

/// A value only the Pseudonym Certificate Authority may open.
///
/// The enforcement of 05-protocols §3.2's "the RA learns nothing it should not": a
/// pre-linkage value travels from a Linkage Authority to the PCA *through* the
/// Registration Authority, and the RA is the one party on that path that must not be able
/// to read it. Making that a type rather than a convention means the RA's code physically
/// cannot get the value out — [`SealedForPca::open`] refuses any opener that is not the
/// PCA node — and `tests/privacy.rs` checks the refusal rather than assuming it.
#[derive(Debug, Clone)]
pub struct SealedForPca<T> {
    inner: T,
}

impl<T> SealedForPca<T> {
    /// Seals a value for the PCA.
    pub const fn seal(inner: T) -> SealedForPca<T> {
        SealedForPca { inner }
    }

    /// Opens it, if `opener` is the PCA.
    pub fn open(self, opener: NodeId, pca: NodeId) -> Option<T> {
        (opener == pca).then_some(self.inner)
    }

    /// The bytes an ECIES-encrypted wrapper adds, whatever is inside.
    pub const fn wrapper_bytes() -> u32 {
        ECIES_P256_ENCRYPTED_KEY_BYTES
    }
}

/// A linkage-chain identifier: the handle an LA knows a device's seed chain by.
///
/// Opaque by construction. The RA holds a pair of them per device and the MA is told them
/// during an investigation; neither can derive a linkage value from one, because the seed
/// never leaves the LA until a revocation decision ([BRECHT §VI-C, §VI-D]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Lci(pub u64);

/// Which of the two Linkage Authorities.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum LaIndex {
    /// LA1.
    One,
    /// LA2.
    Two,
}

impl LaIndex {
    /// The index into a two-element array.
    pub const fn idx(self) -> usize {
        match self {
            LaIndex::One => 0,
            LaIndex::Two => 1,
        }
    }
}

/// The butterfly request material a device uploads.
#[derive(Debug, Clone)]
pub struct ProvisioningRequest {
    /// The device.
    pub device: NodeId,
    /// The caterpillar signing public key `A`.
    pub signing_public: Point,
    /// The caterpillar encryption public key `P`.
    pub encryption_public: Point,
    /// The signing expansion key `ck`.
    pub ck: ExpansionKey,
    /// The encryption expansion key `ek`.
    pub ek: ExpansionKey,
    /// The first i-period requested.
    pub start_i: u32,
    /// How many i-periods.
    pub periods: u32,
    /// Certificates per i-period (`j` runs `0..jmax`).
    pub jmax: u32,
}

/// One certified pseudonym credential as it reaches the device.
#[derive(Debug, Clone)]
pub struct IssuedCredential {
    /// The i-period.
    pub i: u32,
    /// The index within the period.
    pub j: u32,
    /// The linkage value in the certificate's `linkageData`.
    pub lv: LinkageValue,
    /// The certified public key the PCA put in the certificate.
    pub certified_public: Point,
    /// The PCA's secret randomiser, which reaches the device encrypted to its cocoon
    /// encryption key and which it needs to derive the matching private key.
    pub c: Scalar,
}

/// One request the RA sends the PCA: a cocoon key pair and two sealed pre-linkage values.
#[derive(Debug, Clone)]
pub struct CertRequestItem {
    /// The i-period.
    pub i: u32,
    /// The index within the period.
    pub j: u32,
    /// The cocoon signing key `B`.
    pub cocoon_signing: Point,
    /// LA1's pre-linkage value, sealed.
    pub plv1: SealedForPca<PreLinkageValue>,
    /// LA2's pre-linkage value, sealed.
    pub plv2: SealedForPca<PreLinkageValue>,
    /// LA1's chain identifier, sealed: the PCA stores it so an investigation can start.
    pub lci1: SealedForPca<Lci>,
    /// LA2's chain identifier, sealed.
    pub lci2: SealedForPca<Lci>,
    /// The hash of the device's provisioning request, which is all that ties the
    /// certificate back to the RA's record.
    pub request_hash: [u8; 32],
}

/// What one Linkage Authority answers a pre-linkage request with.
#[derive(Debug, Clone)]
pub struct PreLinkageBatch {
    /// Which LA.
    pub la: LaIndex,
    /// Its identifier.
    pub la_id: LaId,
    /// The chain it allocated for this device.
    pub lci: Lci,
    /// `(i, j, plv)`, sealed for the PCA, in ascending `(i, j)`.
    pub values: Vec<(u32, u32, SealedForPca<PreLinkageValue>)>,
}

/// A misbehaviour report as it leaves a device.
#[derive(Debug, Clone)]
pub struct ReportSubmission {
    /// Who reported.
    pub reporter: NodeId,
    /// The i-period of the accused certificate.
    pub subject_i: u32,
    /// The linkage value of the accused certificate — the only stable handle a reporter
    /// has on a pseudonymous device.
    pub subject_lv: LinkageValue,
    /// When the observation was made.
    pub observed_at: v2xw_core::time::SimTime,
}

/// What the PCA answers a `lv → ?` lookup with ([BRECHT §VI-C]).
#[derive(Debug, Clone, Copy)]
pub struct PcaLookup {
    /// LA1's chain identifier for the device that holds this linkage value.
    pub lci1: Lci,
    /// LA2's chain identifier.
    pub lci2: Lci,
    /// The provisioning request the certificate came from.
    pub request_hash: [u8; 32],
}

/// The messages of the CAMP SCMS flows.
///
/// Boxed where a payload is large, so the enum stays small enough to move cheaply.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ScmsMsg {
    /// Device → DCM: bootstrap request with the canonical key.
    EnrolRequest {
        /// The device.
        device: NodeId,
    },
    /// DCM → ECA.
    EnrolForward {
        /// The device.
        device: NodeId,
    },
    /// ECA → device: the enrolment certificate and the trust anchors.
    EnrolResponse {
        /// The device.
        device: NodeId,
    },
    /// Device → LOP → RA: the butterfly provisioning request.
    ProvisioningRequest(Box<ProvisioningRequest>),
    /// RA → device: request hash, first batch time, repository URL.
    ProvisioningAck {
        /// The device.
        device: NodeId,
        /// The hash the RA will know this request by.
        request_hash: [u8; 32],
    },
    /// RA → LA: allocate a chain and return pre-linkage values.
    PreLinkageRequest {
        /// The device the RA is provisioning (the LA learns only that some request needs
        /// a chain; it never sees the device's keys).
        device: NodeId,
        /// Which LA.
        la: LaIndex,
        /// First i-period.
        start_i: u32,
        /// How many periods.
        periods: u32,
        /// Certificates per period.
        jmax: u32,
    },
    /// LA → RA: the sealed pre-linkage values.
    PreLinkageResponse(Box<PreLinkageBatch>),
    /// The RA's shuffle window closing.
    ShuffleTimer,
    /// RA → PCA: one i-period of certificate requests.
    CertRequest {
        /// The device (carried for the simulation's bookkeeping; the PCA learns only the
        /// request hash, and `tests/privacy.rs` checks that its state records no more).
        device: NodeId,
        /// The i-period.
        i: u32,
        /// The requests.
        items: Box<Vec<CertRequestItem>>,
    },
    /// PCA → RA: the certified credentials, sealed for the device.
    CertResponse {
        /// The device.
        device: NodeId,
        /// The i-period.
        i: u32,
        /// The credentials.
        credentials: Box<Vec<IssuedCredential>>,
    },
    /// Device → RA: fetch one batch.
    BatchDownloadRequest {
        /// The device.
        device: NodeId,
        /// The i-period.
        i: u32,
    },
    /// RA → device: the batch.
    BatchDownload {
        /// The device.
        device: NodeId,
        /// The i-period.
        i: u32,
        /// The credentials.
        credentials: Box<Vec<IssuedCredential>>,
    },
    /// Device → LOP → RA → MA: a misbehaviour report.
    Report(Box<ReportSubmission>),
    /// The report shuffle window closing.
    ReportShuffleTimer,
    /// MA → PCA: which device holds this linkage value?
    PcaLookupRequest {
        /// The i-period.
        i: u32,
        /// The linkage value.
        lv: LinkageValue,
    },
    /// PCA → MA.
    PcaLookupResponse {
        /// The answer, absent if the PCA never issued such a certificate.
        found: Option<PcaLookup>,
    },
    /// MA → LA: are these two chains the same device?
    SameDeviceRequest {
        /// Which LA.
        la: LaIndex,
        /// One chain.
        a: Lci,
        /// The other.
        b: Lci,
    },
    /// LA → MA: a boolean, and nothing else ([BRECHT §VI-C]).
    SameDeviceResponse {
        /// Which LA answered.
        la: LaIndex,
        /// Whether the two chains belong to one device.
        same: bool,
    },
    /// MA → RA: blocklist the enrolment certificate behind this request hash.
    BlocklistRequest {
        /// The request hash.
        request_hash: [u8; 32],
    },
    /// RA → MA: the LA hosts and chain identifiers ([BRECHT §VI-D]).
    BlocklistResponse {
        /// Whether the RA knew the hash.
        known: bool,
        /// LA1's chain.
        lci1: Lci,
        /// LA2's chain.
        lci2: Lci,
    },
    /// MA → LA: the seed for the revocation period.
    SeedRequest {
        /// Which LA.
        la: LaIndex,
        /// The chain.
        lci: Lci,
        /// The first revoked period.
        i: u32,
    },
    /// LA → MA: `ls_x(i)` and nothing earlier.
    SeedResponse {
        /// Which LA.
        la: LaIndex,
        /// The LA's identifier, which the CRL entry carries.
        la_id: LaId,
        /// The seed at period `i`.
        seed: LinkageSeed,
        /// The period.
        i: u32,
    },
    /// MA → CRL Generator: add an entry and issue.
    CrlAppend {
        /// The entry's first revoked period.
        i: u32,
        /// LA1's identifier and seed.
        la1: (LaId, LinkageSeed),
        /// LA2's identifier and seed.
        la2: (LaId, LinkageSeed),
        /// Certificates per period.
        jmax: u32,
    },
    /// CRL Generator → CRL Store / CRL Broadcast: the signed list.
    CrlPublish {
        /// How many entries it carries.
        entries: u32,
    },
    /// Device → CRL Store: fetch the current list.
    CrlDownloadRequest {
        /// The device.
        device: NodeId,
    },
    /// CRL Store → device: the list.
    CrlDownload {
        /// The device.
        device: NodeId,
        /// How many entries.
        entries: u32,
    },
}

/// Every SCMS message size, in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScmsSizes {
    /// Certificate sizes from the real encoder.
    pub certs: CertificateSizes,
    /// The five uncited sizes, as card parameters.
    pub params: SizeParams,
}

const DERIVATION: &str = "components: 04-models.md §9.1 (measured envelope overhead), SEC 1 §2.3.3, FIPS 197 §5, \
     FIPS 180-4, IEEE 1609.2 §6.3.26, Ieee1609Dot2BaseTypes.asn; structure: 05-protocols.md \
     §3.2 / Brecht 2018 §V-E, §VI-C, §VI-D";

impl ScmsSizes {
    /// The sizes for a deployment.
    pub const fn new(certs: CertificateSizes, params: SizeParams) -> ScmsSizes {
        ScmsSizes { certs, params }
    }

    /// Device → DCM: the canonical verification key under a digest-signed envelope.
    pub const fn enrol_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + EC_POINT_COMPRESSED_BYTES + TIME32_BYTES,
            "envelope(digest) + canonical public key + time32",
            DERIVATION,
        )
    }

    /// DCM → ECA: the same, re-signed by the DCM with its certificate attached.
    pub fn enrol_forward(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + EC_POINT_COMPRESSED_BYTES
                + TIME32_BYTES,
            "envelope(certificate) + DCM certificate + canonical public key + time32",
            DERIVATION,
        )
    }

    /// ECA → device: the enrolment certificate under the ECA's own certificate.
    pub fn enrol_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + self.certs.enrolment.bytes(),
            "envelope(certificate) + ECA certificate + enrolment certificate",
            DERIVATION,
        )
    }

    /// Device → RA: `{A, P, ck, ek, time, start}` signed with the enrolment certificate.
    ///
    /// 05-protocols §3.2 estimates this at 300–400 B; this is the same structure summed
    /// from its components with the certificate measured rather than estimated.
    pub fn provisioning_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.enrolment.bytes()
                + 2 * EC_POINT_COMPRESSED_BYTES
                + 2 * AES128_KEY_BYTES
                + 2 * TIME32_BYTES,
            "envelope(certificate) + enrolment certificate + A + P + ck + ek + time + start",
            DERIVATION,
        )
    }

    /// RA → device: `{hash8(request), first-batch time, repository URL}`.
    ///
    /// Dominated by the URL, which no source gives, so the whole size is carried as the
    /// `repo_url_bytes` parameter.
    pub const fn provisioning_ack(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + HASHED_ID8_BYTES
                + TIME32_BYTES
                + self.params.repo_url_bytes,
            "repo_url_bytes",
        )
    }

    /// RA → LA: a chain request.
    pub const fn pre_linkage_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + self.params.linkage_chain_identifier_bytes
                + 3 * TIME32_BYTES,
            "linkage_chain_identifier_bytes",
        )
    }

    /// LA → RA: `n` sealed pre-linkage values.
    pub const fn pre_linkage_response(&self, n: u32) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + self.params.linkage_chain_identifier_bytes
                + n * (LINKAGE_VALUE_BYTES + ECIES_P256_ENCRYPTED_KEY_BYTES),
            "linkage_chain_identifier_bytes",
        )
    }

    /// RA → PCA: `n` certificate requests.
    pub const fn cert_request(&self, n: u32) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + n * (EC_POINT_COMPRESSED_BYTES
                    + 2 * (LINKAGE_VALUE_BYTES + ECIES_P256_ENCRYPTED_KEY_BYTES)
                    + 2 * (self.params.linkage_chain_identifier_bytes
                        + ECIES_P256_ENCRYPTED_KEY_BYTES)
                    + SHA256_BYTES),
            "linkage_chain_identifier_bytes",
        )
    }

    /// PCA → RA, and RA → device: `n` certificates, each with its sealed randomiser.
    pub const fn cert_batch(&self, n: u32) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + n * (self.certs.pseudonym.bytes() + ECIES_P256_ENCRYPTED_KEY_BYTES),
            "envelope(digest) + n · (pseudonym certificate + ECIES wrapper)",
            DERIVATION,
        )
    }

    /// RA → device: one batch in its container.
    pub const fn batch_download(&self, n: u32) -> WireSize {
        WireSize::parameter(
            self.params.batch_container_bytes
                + ENVELOPE_OVERHEAD_DIGEST_BYTES
                + n * (self.certs.pseudonym.bytes() + ECIES_P256_ENCRYPTED_KEY_BYTES),
            "batch_container_bytes",
        )
    }

    /// Device → RA: a batch request.
    pub const fn batch_download_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + HASHED_ID8_BYTES + TIME32_BYTES,
            "envelope(digest) + request hash8 + i-period",
            DERIVATION,
        )
    }

    /// A misbehaviour report: TS 103 759 payload, signed with a pseudonym certificate and
    /// encrypted to the MA.
    pub const fn report(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.pseudonym.bytes()
                + self.params.report_payload_bytes
                + ECIES_P256_ENCRYPTED_KEY_BYTES,
            "report_payload_bytes",
        )
    }

    /// MA → PCA: `(i, lv)`.
    pub const fn pca_lookup_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + TIME32_BYTES + LINKAGE_VALUE_BYTES,
            "envelope(digest) + i-period + linkage value",
            DERIVATION,
        )
    }

    /// PCA → MA: two chain identifiers and a request hash.
    pub const fn pca_lookup_response(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + 2 * self.params.linkage_chain_identifier_bytes
                + SHA256_BYTES,
            "linkage_chain_identifier_bytes",
        )
    }

    /// MA → LA: "same device?".
    pub const fn same_device_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + 2 * self.params.linkage_chain_identifier_bytes,
            "linkage_chain_identifier_bytes",
        )
    }

    /// LA → MA: one bit, in an envelope.
    pub const fn same_device_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + 1,
            "envelope(digest) + boolean",
            DERIVATION,
        )
    }

    /// MA → RA: a request hash.
    pub const fn blocklist_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + SHA256_BYTES,
            "envelope(digest) + request hash",
            DERIVATION,
        )
    }

    /// RA → MA: the LA hosts and chains.
    pub const fn blocklist_response(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + 2 * self.params.linkage_chain_identifier_bytes,
            "linkage_chain_identifier_bytes",
        )
    }

    /// MA → LA: a seed request.
    pub const fn seed_request(&self) -> WireSize {
        WireSize::parameter(
            ENVELOPE_OVERHEAD_DIGEST_BYTES
                + self.params.linkage_chain_identifier_bytes
                + TIME32_BYTES,
            "linkage_chain_identifier_bytes",
        )
    }

    /// LA → MA: `ls_x(i)`.
    pub const fn seed_response(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + crate::sizes::LINKAGE_SEED_BYTES + 2 + TIME32_BYTES,
            "envelope(digest) + linkage seed + la_id + i-period",
            DERIVATION,
        )
    }

    /// MA → CRL Generator: one entry's worth of material.
    pub const fn crl_append(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + CRL_LINKAGE_ENTRY_BYTES,
            "envelope(digest) + one linkage entry",
            DERIVATION,
        )
    }

    /// A signed CRL of `entries` entries.
    ///
    /// 40 B per entry is Brecht 2018 §VI-F's figure — 32 B of seeds plus group overhead —
    /// and the 10,000-entry list it gives as ≈ 400 kB comes back out of this expression.
    pub const fn crl(&self, entries: u32) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_CERT_BYTES
                + self.certs.authority.bytes()
                + entries * CRL_LINKAGE_ENTRY_BYTES,
            "envelope(certificate) + CRLG certificate + entries · 40 B",
            DERIVATION,
        )
    }

    /// Device → CRL Store: a fetch.
    pub const fn crl_request(&self) -> WireSize {
        WireSize::derived(
            ENVELOPE_OVERHEAD_DIGEST_BYTES + TIME32_BYTES,
            "envelope(digest) + last-known CRL time",
            DERIVATION,
        )
    }

    /// Every message kind with a representative size, for the conformance test.
    ///
    /// The step names are the ones the flows pass to `proto.msg`, so
    /// `tests/wire_sizes.rs` can check that no step ever put bytes on a link without an
    /// entry here.
    pub fn table(&self) -> Vec<(&'static str, WireSize)> {
        vec![
            ("enrol-request", self.enrol_request()),
            ("enrol-forward", self.enrol_forward()),
            ("enrol-response", self.enrol_response()),
            ("provisioning-request", self.provisioning_request()),
            ("provisioning-request-proxied", self.provisioning_request()),
            ("provisioning-ack", self.provisioning_ack()),
            ("pre-linkage-request", self.pre_linkage_request()),
            ("pre-linkage-response", self.pre_linkage_response(1)),
            ("cert-request", self.cert_request(1)),
            ("cert-response", self.cert_batch(1)),
            ("batch-download-request", self.batch_download_request()),
            ("batch-download", self.batch_download(1)),
            ("report", self.report()),
            ("report-proxied", self.report()),
            ("report-forward", self.report()),
            ("pca-lookup-request", self.pca_lookup_request()),
            ("pca-lookup-response", self.pca_lookup_response()),
            ("same-device-request", self.same_device_request()),
            ("same-device-response", self.same_device_response()),
            ("blocklist-request", self.blocklist_request()),
            ("blocklist-response", self.blocklist_response()),
            ("seed-request", self.seed_request()),
            ("seed-response", self.seed_response()),
            ("crl-append", self.crl_append()),
            ("crl-publish", self.crl(1)),
            ("crl-broadcast", self.crl(1)),
            ("crl-request", self.crl_request()),
            ("crl-download", self.crl(1)),
        ]
    }
}

/// A device's own butterfly state, kept out of the message module's public surface only
/// because it never travels: `a`, `p` and the derived private keys stay on the device.
#[derive(Debug, Clone)]
pub struct DeviceKeys {
    /// The caterpillar.
    pub caterpillar: Caterpillar,
}
