//! Car-following models — 04-models.md §2.1.
//!
//! One model is implemented natively at the medium tier, `mobility/car-following/idm`
//! ([`idm`]). §2.1 also catalogues Krauss, Wiedemann 99 and Gipps; none of the three is
//! implemented here, and each is left out for a reason the document itself records:
//!
//! * **Krauss** needs the safe-velocity expression transcribed from
//!   `MSCFModel_Krauss.cpp` (EPL-2.0, read only, never vendored) and quoted in its card.
//!   §2.1 says the expression "is not in the research sheets", so writing one from memory
//!   would be inventing the model.
//! * **Wiedemann 99** has seven of its ten thresholds marked `TODO: calibrate` in §2.1,
//!   including every one that shapes the braking reaction. A model whose behaviour is
//!   dominated by uncited numbers is not a model.
//! * **Gipps** has *all* of its parameters marked `TODO: calibrate` (§2.1: "obtain Gipps
//!   1981 Transportation Research B 15(2) and record its Table values").
//!
//! The calibration plans for all three are in §2.1 and are unchanged by this crate.

pub mod idm;

pub use idm::{Idm, IdmParams, IdmPreset};
