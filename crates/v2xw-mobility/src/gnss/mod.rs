//! GNSS error models — 04-models.md §2.10 and §3.8.
//!
//! * [`gauss_markov`] — `gnss/error/gauss-markov`, the **default** for the medium and high
//!   tiers: a first-order Gauss-Markov error per axis whose σ and τ are *fitted* to the
//!   measured Reid 2019 quantiles at construction, with the fit and its residual on the
//!   card (§3.8).
//! * [`ou_bias_legacy`] — `gnss/error/ou-bias-legacy`, the frozen reference engine's model,
//!   kept as the abstract-tier preset and the parity oracle (§2.10).
//!
//! # The defect both models fix
//!
//! §2.10 records it: the legacy engine computed the broadcast confidence from the
//! **realised bias** — `conf = 2.448·sqrt(σ_nom² + bias_x² + bias_y²)` — which is an oracle
//! quantity. A real receiver cannot see its own bias; if it could, it would subtract it.
//! Both models here compute the ellipse from the model's *own noise parameters*
//! (`σ_nom² + σ_b²`, a variance the receiver knows because it is a property of the
//! receiver), never from the sampled error. [`common::confidence_95_m`] is the one place
//! that arithmetic happens, and the test
//! `ou_bias_legacy::tests::the_confidence_does_not_leak_the_true_bias` pins it.

pub mod common;
pub mod gauss_markov;
pub mod ou_bias_legacy;

pub use common::{RAYLEIGH_95, confidence_95_m, env_scale, fit_scale};
pub use gauss_markov::{GaussMarkovGnss, GaussMarkovParams, QuantileFit};
pub use ou_bias_legacy::{LegacyGnss, LegacyGnssParams};
