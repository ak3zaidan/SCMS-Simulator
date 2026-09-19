//! Clock models — 04-models.md §2.10 and §3.8.
//!
//! * [`tcxo_ocxo`] — `clock/drift/tcxo-ocxo`, the medium and high tiers' default: GNSS time
//!   while the fix is valid, oscillator drift in holdover.
//! * [`none`] — `clock/drift/none`, the abstract tier: node time equals GNSS time,
//!   drift-free.

pub mod none;
pub mod tcxo_ocxo;

pub use none::DriftFreeClock;
pub use tcxo_ocxo::{Oscillator, OscillatorClock, OscillatorParams};
