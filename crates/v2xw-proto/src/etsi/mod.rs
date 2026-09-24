//! `protocol/etsi/ts102941` — the ETSI ITS PKI.
//!
//! # Build decision D5, and which of its two options this takes
//!
//! D5 records that the ETSI PKI ASN.1 "fails codegen on `WITH COMPONENTS` inner subtyping
//! at `EtsiTs102941MessagesItss.asn:105:6`" and defers it, so there is no encoder for
//! `EtsiTs102941Data` and there will not be one in this phase. D5 offers two ways forward:
//! hand-written structures, or the size model. **This uses hand-written structures**, and
//! the split is worth stating precisely, because it is not "everything is modelled":
//!
//! * every **certificate** inside every message — the EA's, the AA's, the TLM's, the Root
//!   CA's, an enrolment credential, an authorization ticket — is sized by the **real COER
//!   encoder** in `v2xw-sec`, through [`crate::sizes::CertificateSizes`]. A change to the
//!   certificate profile moves every trust list and every ticket batch with it;
//! * every **cryptographic component** — a compressed P-256 point, an ECDSA signature in
//!   the 1609.2 COER shape, an ECIES wrapper, an AES-128 key, a `HashedId8`, a `Time32` —
//!   is a cited constant from [`crate::sizes`];
//! * only the **framing** no clause fixes is a card parameter with a calibration plan:
//!   the `InnerEcRequest` subject-attribute container, the butterfly acknowledgement's
//!   `currentI`/`requestHash`/`nextDlTime` block, the trust-list container, and the
//!   TS 103 759 report payload. `tests/wire_sizes.rs` walks every message kind and checks
//!   that each of those names a parameter that really is on the card with a real plan, so
//!   the variant cannot smuggle an invented number past invariant I-P8.
//!
//! D5 also says the flow shapes matter more than the byte encodings here, and that is what
//! the module is organised around: what is modelled is who talks to whom, in what order,
//! how many signatures each hop costs, and what each stage's timestamp is.
//!
//! # What runs
//!
//! | Flow | Clause | Shape |
//! |---|---|---|
//! | [`FlowId::EtsiEnrolment`] | §6.2.3.2 | ITS-S → EA → ITS-S |
//! | [`FlowId::EtsiAuthorization`] | §6.2.3.3-4 | ITS-S → AA → EA → AA → ITS-S |
//! | [`FlowId::EtsiButterflyAuthorization`] | §6.2.3.5, Fig. 23 | ITS-S → EA (one request), EA → AA (a batch), AA → EA |
//! | [`FlowId::EtsiAtDownload`] | §6.2.3.5 | ITS-S → EA → ITS-S, at `nextDlTime` |
//! | [`FlowId::EtsiTrustList`] | §6.3.1-6.3.3 | TLM → CPOC → ITS-S |
//! | [`FlowId::EtsiCaCrl`] | §6.3.5 | RCA → CPOC → ITS-S |
//! | [`FlowId::EtsiMisbehaviourReport`] | TS 103 759 §4-7 | ITS-S → MA → (pre-processing) → EA blocklist |
//!
//! [`FlowId::EtsiEnrolment`]: crate::stage::FlowId::EtsiEnrolment
//! [`FlowId::EtsiAuthorization`]: crate::stage::FlowId::EtsiAuthorization
//! [`FlowId::EtsiButterflyAuthorization`]: crate::stage::FlowId::EtsiButterflyAuthorization
//! [`FlowId::EtsiAtDownload`]: crate::stage::FlowId::EtsiAtDownload
//! [`FlowId::EtsiTrustList`]: crate::stage::FlowId::EtsiTrustList
//! [`FlowId::EtsiCaCrl`]: crate::stage::FlowId::EtsiCaCrl
//! [`FlowId::EtsiMisbehaviourReport`]: crate::stage::FlowId::EtsiMisbehaviourReport
//!
//! # Why the butterfly variant shares the SCMS's arithmetic and not its code
//!
//! 05-protocols.md §4.2 notes that "both variants share the SCMS butterfly arithmetic, so
//! the same ported code serves both protocols", and it does: the caterpillar-to-cocoon
//! expansion and the linkage construction live in `v2xw-sec` and are called by
//! [`crate::scms`]. What differs here is *who holds what*, and it is the difference a
//! privacy study is about — in the SCMS the Registration Authority expands and the
//! Pseudonym CA certifies without seeing the requester; in ETSI the **Enrolment**
//! Authority expands and the **Authorization** Authority certifies, and the ticket batch
//! waits at the EA rather than in a device repository. So this module models the ETSI
//! *custody* of the batch and does not re-implement the arithmetic.
//!
//! # What is still declared and not built
//!
//! * the RSU delta-CTL broadcast of Annex D.3, which is an air interface and therefore
//!   `v2xw-node`'s roadside runtime and `v2xw-radio`'s, not this crate's;
//! * the manufacturer SOC interface of §6.2.2, which happens before enrolment;
//! * the Distribution Centre as a distinct hop: it is a cache in front of the CPOC and
//!   adding it would add a link and no decision.

pub mod ts102941;

pub use ts102941::{
    CA_CRL_REVOCATION, DEFERRED_FLOWS, ETSI_TS102941_ID, EtsiNodes, EtsiParams, EtsiRun, EtsiSizes,
    EtsiTs102941, FLOWS, PASSIVE_REVOCATION, SEPARATIONS, Ts102941Msg, all_flows,
};
