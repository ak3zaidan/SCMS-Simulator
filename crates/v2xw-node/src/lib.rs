//! `v2xw-node` — Node runtime: queues, servers (CPU/HSM), stores, clock, position estimate, telemetry.
//!
//! Never touches: ground-truth kinematics (it receives only what the GNSS model gives) (02-architecture.md §2, ADR 0010).
//!
//! Phase 0 stub: the responsibility is fixed, the implementation lands in a
//! later phase of the roadmap (docs/design/10-roadmap.md).
