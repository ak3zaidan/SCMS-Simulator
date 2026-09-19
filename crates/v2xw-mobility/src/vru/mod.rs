//! VRU mobility — 04-models.md §2.5.
//!
//! * [`social_force`] — `vru/pedestrian/social-force`, the medium tier: the Helbing and
//!   Molnár 1995 force model, walking the world's sidewalk and crossing lanes.
//!
//! `striping-sumo` is SUMO's and belongs to the high tier; §2.5 records its parameters for
//! the adapter. `vru/cyclist/lane-follow` is the IDM on a bike lane with the `bicycle`
//! class of §2.7, which is [`crate::carfollowing::idm`] plus
//! [`crate::classes::VehicleClass::Bicycle`] and needs no model of its own.

pub mod social_force;

pub use social_force::{SocialForce, SocialForceParams};
