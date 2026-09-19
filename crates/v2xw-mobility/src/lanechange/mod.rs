//! Lane-change models — 04-models.md §2.2.
//!
//! `mobility/lane-change/mobil` ([`mobil`]) is the native medium tier. `lc2013` is SUMO's
//! and belongs to the high tier; §2.2 records its fifteen parameters and their defaults for
//! the adapter that drives SUMO, and nothing of it is reimplemented here.

pub mod mobil;

pub use mobil::{Mobil, MobilParams, MobilPreset, smoothstep, transition_duration};
