//! `v2xw-proto` — Protocol host: entity state machines, flows, revocation, reporting formats.
//!
//! This is the crate that makes the simulator a V2X *security* simulator rather than a
//! traffic simulator with signatures on the messages. The radio crates answer "did the
//! frame arrive"; `v2xw-sec` answers "does the signature verify"; this crate answers the
//! questions the design brief actually asks:
//!
//! * what does it cost to give a fleet pseudonym certificates, in round trips, queueing
//!   delay and bytes;
//! * how long after a detector fires is a misbehaving vehicle actually unable to be
//!   believed, decomposed into every stage of the path;
//! * and what does each party in the credential system learn on the way.
//!
//! # The three plug-ins
//!
//! | Module | What it is | Status |
//! |---|---|---|
//! | [`scms`] | `protocol/scms/camp` — the CAMP SCMS / IEEE 1609.2.1 | complete for the seven flows of 05-protocols §3.2 |
//! | [`etsi`] | `protocol/etsi/ts102941` — the ETSI ITS PKI | enrolment, standard and butterfly authorization, ticket download, ECTL and CA-CRL distribution, TS 103 759 reporting; hand-written framing around real encoder certificate sizes (build decision D5) |
//! | [`threshold`] | the surface an interactive threshold/umbrella protocol needs | trait surface plus one shape proof; the owner's scheme is pending |
//!
//! # The invariants this crate is responsible for
//!
//! The list in 05-protocols §7, plus one of its own:
//!
//! * **I-P1** — no direct entity-to-entity calls. Every message crosses a
//!   [`net::Link`] and pays for its bytes; [`kernel::Kernel::dispatch`] refuses to deliver
//!   between nodes with no link rather than delivering for free.
//! * **I-P2** — every compute step is charged. An entity cannot look at the clock: it
//!   fills an [`kernel::Outbox`] and the kernel decides when its work finished, so
//!   forgetting to charge is not a way to make an entity fast, it is a way to make it
//!   free and visibly wrong in the operation counts.
//! * **I-P3** — every flow declares which credential signs what, in
//!   [`spec::CredentialTypeSpec`] and the plug-in's signing parameters.
//! * **I-P4** — every flow declares the stage timestamps it emits, in
//!   [`spec::FlowSpec::stages`], and `tests/flows.rs` asserts the emitted stages equal the
//!   declaration in order. A flow that stops emitting a stage fails a test.
//! * **I-P5** — end entities read no backend state. A device's knowledge is its own
//!   [`scms::run::DeviceState`]; the backend's answers reach it only as messages.
//! * **I-P8** (this crate's own) — no byte count reaches a link without a
//!   [`sizes::SizeProvenance`]: the real encoder, a cited constant, or a model-card
//!   parameter with a calibration plan. `tests/wire_sizes.rs` walks every message kind and
//!   checks it, including that each named parameter really is on the card with a plan.
//!
//! # The security property that is tested rather than assumed
//!
//! Linkage-based revocation must be **forward-only**: publishing a revoked device's seeds
//! for period `i` must not make its certificates from before `i` linkable. The mechanism
//! is in `v2xw-sec` — a Linkage Authority releases `ls(i)`, never `ls(0)`, and
//! [`v2xw_sec::linkage::CrlLinkageEntry::matches`] refuses an earlier period outright —
//! and this crate exercises it end to end: `tests/backward_privacy.rs` provisions a device
//! across several i-periods, revokes it from one of them, distributes the CRL through the
//! real flow and asserts that the device's earlier certificates are not matched by the CRL
//! it downloaded, while every later one is.
//!
//! # Determinism
//!
//! No wall clock is read. No `std::collections::HashMap` is iterated. Every service time
//! and link delay is integer nanoseconds, so no float is exported and the quantisation
//! rule of build decision D9 has nothing to bite on. All randomness — the device's
//! caterpillar seed, a Linkage Authority's initial seed, the PCA's per-certificate
//! randomiser — comes from [`v2xw_core::rng::RngRegistry`] streams keyed by
//! `(RngDomain::Crypto, EntityRef::Node(..))`. Events are ordered by `(time, creation
//! sequence)`, a total order.
//!
//! # What is deliberately not here
//!
//! The engine's own event loop. `v2xw-engine` is being built in parallel and its event
//! payload type does not exist yet, so [`kernel`] carries the smallest kernel that keeps
//! I-P1 and I-P2 true, and the entity logic in [`scms::run`] is written against an
//! [`kernel::Outbox`] that is the accumulator form of 03-interfaces §7's `Action` list. The
//! engine adopts these entities by dispatching the same outboxes on its own scheduler; no
//! protocol logic has to move.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod error;
pub mod etsi;
pub mod kernel;
pub mod net;
pub mod overhead;
pub mod pseudonym;
pub mod scms;
pub mod service;
pub mod sim;
pub mod sizes;
pub mod spec;
pub mod stage;
pub mod threshold;

pub use error::{ProtoError, Result};
pub use kernel::{Delivery, Kernel, Outbox};
pub use net::{BackendNet, Link, Transport};
pub use overhead::{AirInterface, MessageOverhead, OverheadProfile, Payload};
pub use pseudonym::{CertEvent, ChangeReason, ChangeTrigger, PseudonymStore, PseudonymStrategy};
pub use scms::{CAMP_SCMS_ID, CampScms, ScmsNodes, ScmsParams, ScmsRun};
pub use service::{BatchPolicy, ServiceModelSpec, ServiceQueue};
pub use sim::{
    Bootstrap, CredentialService, Drained, ProvisioningCost, Pseudonym, RevocationLatency,
};
pub use sizes::{CertificateSizes, SizeParams, SizeProvenance, WireSize};
pub use spec::{
    ActiveRevocation, Centrality, CredState, CredentialTypeSpec, EntityRoleSpec, FlowSpec,
    HolderKind, OpDescriptor, PassiveRevocation, ProtocolId, RevocationMechanism, SeparationRule,
    StorageCounter, TrustBoundary, ValidityPolicy, separation_violations,
};
pub use stage::{FlowId, FlowRun, StageId, StageLog, StageStamp, WireStep};
pub use threshold::{
    Committee, FrostShapedPlaceholder, RefreshPolicy, RoundMessage, RoundPlan, ThresholdOp,
    ThresholdProtocol, UmbrellaScheme,
};

/// Registers every model this crate provides.
///
/// # Errors
/// Whatever the registry returns: a card that does not validate, or a duplicate id.
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    use std::sync::Arc;
    let mut refs = scms::register(registry)?;
    refs.push(registry.register_model(Arc::new(etsi::EtsiTs102941::default()))?);
    Ok(refs)
}
