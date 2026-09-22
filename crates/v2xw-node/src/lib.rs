//! `v2xw-node` — the node runtime: what turns a vehicle into a communicating station.
//!
//! A node owns its beliefs and nothing else. It knows what its GNSS receiver told it,
//! what its oscillator says the time is, what it has received and what it holds in its
//! stores. It does not know where it is, what time it is, or who sent the message it is
//! looking at, and every number this crate produces is a number a fielded receiver could
//! also have produced (02-architecture.md §2, ADR 0010, invariant I-C2).
//!
//! # The three things this crate is for
//!
//! **Service time is real.** A node signs on the hardware its profile describes. The
//! reference OBU's secure element publishes `<9 ms signing latency` and `>110
//! signatures/s`; at 10 Hz that is 9 % of one engine, and a pseudonym change inside the
//! same 100 ms window has to fit beside it. A node under a hundred neighbours at 10 Hz
//! needs a kilohertz of verification throughput, and one of the profiles in
//! 06-node-models.md §7 documents a real automotive part that offers a hundred. So a node
//! here queues, delays and drops, and [`telemetry`] reports which of the three happened —
//! because that is the phenomenon the whole simulator exists to study, and a model with
//! instantaneous cryptography cannot produce it.
//!
//! **Nothing is invented.** A hardware profile is data with a citation per number
//! ([`profile`], [`profiles`]); a field no vendor publishes carries the measurement that
//! would publish it, and [`profiles::todo_calibrate_total`] counts them. An operation the
//! profile does not cost has *no* service time, not a plausible one: a default would set
//! the load of every run that forgot to configure it, invisibly.
//!
//! **The firewall is enforced, not asserted.** Build decision D11 §3 records that the
//! `NodeView` boundary is strong but not compile-proof and that enforcement belongs in a
//! conformance sentinel. [`firewall`] is that sentinel, `tests/firewall_sentinel.rs`
//! runs it over this crate's own source, and both name the faults that were injected to
//! prove they can go red.
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | Hardware profiles: the schema, and the rule that nothing is invented | [`profile`] | 06-node-models.md §1 |
//! | The ten shipped profiles and the todo-calibrate page | [`profiles`] | 06-node-models.md §7 |
//! | The narrowed engine context, with no ground truth on it | [`ctx`] | build decisions D12.2, D11 |
//! | The node's own clock, and how it drifts | [`clock`] | 06-node-models.md §2.3 |
//! | Bounded queues and drops by cause | [`queue`] | 06-node-models.md §2.1, §2.4 |
//! | CPU and HSM servers, and what an operation costs | [`server`] | 06-node-models.md §2.1, 03-interfaces.md §8 |
//! | Stores, pseudonym rotation, neighbour ageing, the bounded CRL check | [`stores`] | 06-node-models.md §2.2, 05-protocols.md §2 |
//! | verify-all, on-demand, prioritized | [`policy`] | 06-node-models.md §2.1, 03-interfaces.md §6 |
//! | BSM at 10 Hz, CAM on the EN 302 637-2 triggers | [`generate`] | EN 302 637-2 §6.1.3, SAE J2945/1 |
//! | The 208-byte `NodeTelemetry` record | [`telemetry`] | vwp-v1 §3.5.2 |
//! | The step function and the `NodeView` implementation | [`runtime`] | 06-node-models.md §2 |
//! | The ground-truth conformance sentinel | [`firewall`] | 03-interfaces.md §17, D11 |
//!
//! # What is not here yet
//!
//! Stated rather than stubbed, so that a reader can tell a gap from an omission:
//!
//! * **The credential protocol.** Top-up, enrolment and the SCMS/ETSI flows belong to a
//!   `CredentialProtocol` plug-in (03-interfaces.md §7). [`stores::CertStore`] holds what
//!   such a protocol issues and rotates it; nothing here requests a batch, so
//!   `next_topup_ns` is the "none scheduled" sentinel.
//! * **RSU and backend runtimes.** 06-node-models.md §3 and §4 describe an RSU as "the
//!   same queue/server structure as the OBU with a larger profile" plus roles and failure
//!   states, and a backend entity as a `ServiceModel` with batching and availability. The
//!   profiles for both ship here ([`profiles`]); the runtimes do not.
//! * **Perception** (§6), **air time** (the PHY's to compute) and **safety-application
//!   relevance scores**, which the `on-demand` policy consumes and which no application
//!   in this crate produces.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod clock;
pub mod ctx;
pub mod error;
pub mod firewall;
pub mod generate;
pub mod policy;
pub mod profile;
pub mod profiles;
pub mod queue;
pub mod runtime;
pub mod server;
pub mod stores;
pub mod telemetry;

pub use clock::ClockModel;
pub use ctx::{CoreCtx, NodeCtx, NodeCtxExt, NodeRuntimeCtx};
pub use error::{NodeError, Result};
pub use generate::{MessageSchedule, ServiceSet};
pub use policy::{OnDemand, Prioritized, VerificationPolicy, VerifyAll, VerifyDecision};
pub use profile::{Field, FieldStatus, HardwareProfile, HsmKind, NodeKind, RunsOn, StorageModel};
pub use queue::{DropCause, DropLedger, NodeQueue, QueueKind};
pub use runtime::{NodeConfig, ObuRuntime, RxFrame, StepOutcome, Transmission, VerifiedMessage};
pub use server::{OpClass, OpDescriptor, ProfileServiceModel, ServerBank, ServiceModel};
pub use stores::{
    CertStore, CredState, CredentialHandle, CrlGate, CrlVerdict, Neighbor, NeighborTable,
    PeerCertCache, ReportOutbox, RotationPolicy, Stores, VerificationState,
};
pub use telemetry::{NodeState, TelemetryInputs, TelemetryWindow};

/// Registers every model this crate provides with a [`Registry`].
///
/// The ten hardware profiles and the three verification policies. A model without a
/// validating card cannot be registered (03-interfaces.md §12), so this is also the
/// assertion that every profile's generated card and every policy's card is well formed.
///
/// # Errors
/// [`v2xw_core::registry::RegistryError`] if a card fails validation or an id is taken.
///
/// [`Registry`]: v2xw_core::registry::Registry
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    use std::sync::Arc;
    use v2xw_core::model::ModelHandle;

    let mut refs = profiles::register_all(registry)?;
    let policies: Vec<ModelHandle> = vec![
        Arc::new(VerifyAll::new()),
        Arc::new(OnDemand::new(0.5)),
        Arc::new(Prioritized::new(300.0)),
    ];
    for p in policies {
        refs.push(registry.register_model(p)?);
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::card::{Family, SourceKind};
    use v2xw_core::model::Model;
    use v2xw_core::registry::Registry;

    /// Every model in the crate registers, which means every card validates, every
    /// uncalibrated parameter carries a plan (rule R1), and no two models share an id.
    #[test]
    fn every_model_in_the_crate_registers() {
        let mut registry = Registry::new();
        let refs = register_all(&mut registry).expect("all register");
        assert_eq!(refs.len(), registry.len());
        assert_eq!(refs.len(), profiles::all().len() + 3);
    }

    /// The registry's `todo-calibrate` page is not empty, and every entry on it carries a
    /// plan. An empty page would mean either a perfectly documented hardware set or a
    /// schema that cannot express a gap; it is the second that is worth guarding against.
    #[test]
    fn the_todo_calibrate_page_is_populated_and_every_entry_has_a_plan() {
        let mut registry = Registry::new();
        register_all(&mut registry).expect("all register");

        let mut total = 0usize;
        for p in profiles::all() {
            for param in p.card().todo_calibrate() {
                total += 1;
                assert_eq!(param.source.kind, SourceKind::TodoCalibrate);
                let plan = param.calibration.as_deref().unwrap_or("");
                assert!(
                    plan.len() > 20,
                    "{}::{} has no usable calibration plan",
                    p.id,
                    param.name
                );
            }
        }
        assert_eq!(total, profiles::todo_calibrate_total());
        assert!(
            total > 50,
            "only {total} uncalibrated fields, which is suspicious"
        );
    }

    /// Profiles register under the `hardware-profile` family, which is what a scenario
    /// selects them by.
    #[test]
    fn profiles_register_as_hardware_profiles() {
        for p in profiles::all() {
            assert_eq!(p.card().family, Family::HardwareProfile);
        }
        for pol in [
            VerifyAll::new().card().family,
            OnDemand::new(0.5).card().family,
            Prioritized::new(300.0).card().family,
        ] {
            assert_eq!(pol, Family::VerificationPolicy);
        }
    }
}
