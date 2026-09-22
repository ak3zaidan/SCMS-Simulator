//! The protocol-independent interface: roles, credential types, flows, operations.
//!
//! This is 03-interfaces.md §7 and 05-protocols.md §2 in Rust. A plug-in describes itself
//! with these types; the SCMS plug-in (`crate::scms`), the ETSI skeleton (`crate::etsi`)
//! and the threshold hook (`crate::threshold`) are three fillings of the same shape, which
//! is what §1 of 05-protocols claims one interface can hold.

use v2xw_core::ids::NodeId;
use v2xw_core::time::Duration;
use v2xw_sec::primitive::{PrimitiveCatalogue, PrimitiveId, PrimitiveOpKind};

use crate::service::ServiceModelSpec;
use crate::sizes::WireSize;
use crate::stage::{FlowId, StageId};

/// A protocol's stable id, as a scenario names it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ProtocolId(pub &'static str);

impl core::fmt::Display for ProtocolId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

/// The organisational separation a role belongs to.
///
/// The separations of [BRECHT §X] are constraints between these, not properties of one
/// role: see [`SeparationRule`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TrustBoundary {
    /// The certificate-issuing organisation.
    Issuer,
    /// The registration/front-end organisation.
    Registration,
    /// A linkage authority; the two LAs are separate organisations from each other.
    Linkage(u8),
    /// Misbehaviour investigation.
    Misbehaviour,
    /// Revocation publication.
    Revocation,
    /// Network-identifier obscuring.
    Proxy,
    /// Root/policy management (offline in the PoC).
    Policy,
    /// The end entity itself.
    EndEntity,
}

/// A pair of roles a protocol declares must not share a backend node.
///
/// The scenario validator refuses a topology that co-hosts them unless
/// `security.protocol.params.relax_separation` is set and recorded in the manifest
/// (05-protocols §2.1). The validator lives in the engine; this crate supplies the rules
/// and [`separation_violations`] checks a placement against them, so the check exists and
/// is testable before the loader does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeparationRule {
    /// One role.
    pub a: &'static str,
    /// The other role.
    pub b: &'static str,
    /// Why they must be separate.
    pub reason: &'static str,
}

/// How central a role is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Centrality {
    /// Central by construction; distributing it would change the protocol.
    IntrinsicallyCentral,
    /// Central as deployed, but nothing stops a replica.
    Central,
    /// A committee of `n` of which `t + 1` must act.
    Distributed {
        /// Committee size.
        n: u16,
        /// Threshold: `t + 1` participants are needed.
        t: u16,
    },
}

/// One storage counter that grows as a role does its work ([BRECHT Table II]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageCounter {
    /// What is stored, e.g. `"issued certificate (i, j, lv, cert, request hash)"`.
    pub what: &'static str,
    /// Bytes per item.
    pub bytes_each: WireSize,
}

/// One backend role.
#[derive(Debug, Clone)]
pub struct EntityRoleSpec {
    /// The role's name, e.g. `"RA"`.
    pub name: &'static str,
    /// Which organisation it belongs to.
    pub boundary: TrustBoundary,
    /// How central it is.
    pub central: Centrality,
    /// The hardware profile its costs are read from.
    pub default_profile: &'static str,
    /// Its service model.
    pub default_service: ServiceModelSpec,
    /// What its storage grows with.
    pub storage_growth: Vec<StorageCounter>,
    /// Whether it is offline in the reference deployment (Root CA, electors).
    pub offline: bool,
}

/// What a credential is for, and who holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HolderKind {
    /// A vehicle or other end entity.
    EndEntity,
    /// A backend role.
    Entity(&'static str),
}

/// The validity policy of a credential type (05-protocols §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidityPolicy {
    /// The i-period.
    pub period: Duration,
    /// How far a certificate's validity runs past its period.
    pub overlap: Duration,
    /// How many are usable at once.
    pub concurrent: u32,
    /// How far ahead the device is pre-loaded.
    pub preload: Duration,
}

/// Where a credential is in its life (05-protocols §2.2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum CredState {
    /// Requested, not yet issued.
    Requested,
    /// Issued by the authority, not yet with the device.
    Issued,
    /// Downloaded by the device, not yet in its validity window.
    Downloaded,
    /// In use.
    Active,
    /// Past its validity window.
    Expired,
    /// On a revocation list.
    Revoked,
    /// The device ran out: nothing valid is left and top-up has not arrived.
    Starved,
}

/// One credential type.
#[derive(Debug, Clone)]
pub struct CredentialTypeSpec {
    /// The type's name.
    pub name: &'static str,
    /// Its encoded size, from the real encoder wherever one exists.
    pub encoded: WireSize,
    /// Its validity policy.
    pub validity: ValidityPolicy,
    /// Who holds it.
    pub holder: HolderKind,
    /// Which credential types it covers, for umbrella schemes. Empty for SCMS and ETSI.
    pub covers: Vec<&'static str>,
}

/// One flow: a named message sequence and the stages it is required to emit.
///
/// `stages` is a contract, not documentation: `tests/flows.rs` runs each flow and asserts
/// the stamped stages equal this list, in this order. A flow that stops emitting a stage
/// fails, which is the thing 05-protocols §7's invariant I-P4 asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowSpec {
    /// Which flow.
    pub id: FlowId,
    /// The roles that take part, in the order they first act.
    pub participants: &'static [&'static str],
    /// The stages it emits, in order.
    pub stages: &'static [StageId],
}

/// How revocation works in a protocol (05-protocols §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RevocationMechanism {
    /// A list is produced, distributed, processed and enforced.
    Active(ActiveRevocation),
    /// Issuance stops and the device starves.
    Passive(PassiveRevocation),
    /// Both, as in the SCMS.
    Both(ActiveRevocation, PassiveRevocation),
}

/// The active half of a revocation mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveRevocation {
    /// What one entry is and what it costs on the wire.
    pub entry: WireSize,
    /// Which CRL series the entries go in ([BRECHT §VI-G]: 1 = pseudonym certificates).
    pub series: u16,
    /// How often a list is issued.
    pub cadence: Duration,
    /// The stages it emits.
    pub stages: &'static [StageId],
}

/// The passive half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassiveRevocation {
    /// Which role holds the blocklist.
    pub blocklist_at: &'static str,
    /// The stages it emits.
    pub stages: &'static [StageId],
}

/// One costed operation: a primitive, an operation kind and a count.
///
/// The count matters as much as the primitive: "2·jmax AES per CRL entry per period" is
/// the whole cost story of linkage-based revocation, and a model that charged one AES
/// would be measuring nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpDescriptor {
    /// Which primitive.
    pub primitive: PrimitiveId,
    /// Which operation on it.
    pub kind: PrimitiveOpKind,
    /// How many of them.
    pub count: u32,
}

impl OpDescriptor {
    /// `count` operations of `kind` on `primitive`.
    pub const fn new(primitive: PrimitiveId, kind: PrimitiveOpKind, count: u32) -> OpDescriptor {
        OpDescriptor {
            primitive,
            kind,
            count,
        }
    }

    /// The modelled time this costs on `profile`.
    ///
    /// `None` — not zero — when the catalogue publishes no anchor for that primitive on
    /// that profile. The caller decides what to do with a cost nobody has measured;
    /// [`OpDescriptor::charge`] treats it as zero *and says so*, because SHA-256 and
    /// AES-128 have no anchor on any profile in 04-models §9.4 and pretending otherwise
    /// would be inventing a number.
    pub fn duration(&self, profile: &str) -> Option<Duration> {
        let catalogue = PrimitiveCatalogue::standard();
        let one = catalogue
            .get(self.primitive)?
            .cost_duration(self.kind, profile, catalogue)?;
        Some(one.saturating_mul(u64::from(self.count)))
    }

    /// The time to charge: [`OpDescriptor::duration`] or zero when unbenchmarked.
    pub fn charge(&self, profile: &str) -> Duration {
        self.duration(profile).unwrap_or(Duration::ZERO)
    }
}

/// The AES-128 block permutation, as a primitive this crate charges against.
///
/// It is **not** in `v2xw-sec`'s standard catalogue and it has no cost anchor on any
/// hardware profile in 04-models.md §9.4, so [`OpDescriptor::charge`] returns zero time
/// for it. The operations are still *counted*, because the count — two AES blocks per
/// index per CRL entry per i-period — is the cost story of linkage-based revocation, and
/// it is the count, not an invented microsecond figure, that this crate is entitled to
/// report.
pub const AES128_BLOCK: PrimitiveId = PrimitiveId("primitive/aes-128");

/// Checks a role-to-node placement against a protocol's separation rules.
///
/// Returns the rules that are violated, in declaration order. Empty means the placement
/// is legal.
pub fn separation_violations(
    rules: &[SeparationRule],
    placement: &[(&'static str, NodeId)],
) -> Vec<SeparationRule> {
    let node_of = |role: &str| -> Option<NodeId> {
        placement.iter().find(|(r, _)| *r == role).map(|(_, n)| *n)
    };
    rules
        .iter()
        .copied()
        .filter(|rule| match (node_of(rule.a), node_of(rule.b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        })
        .collect()
}
