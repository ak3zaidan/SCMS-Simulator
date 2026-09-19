//! The crate error type.

use v2xw_core::ids::{ActorId, LaneId};

/// Anything a mobility model can refuse to do.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MobError {
    /// A lane id that the world does not contain.
    #[error("lane {lane} is not in this world")]
    NoSuchLane {
        /// The offending id.
        lane: LaneId,
    },
    /// A lane a vehicle of this class may not use (a car routed onto a sidewalk).
    #[error("lane {lane} ({kind}) does not admit classes {classes}")]
    LaneNotAdmitted {
        /// The lane.
        lane: LaneId,
        /// Its kind, for the message.
        kind: &'static str,
        /// The classes that were asked for.
        classes: String,
    },
    /// No route exists between two lanes for the given class mask.
    #[error("no route from lane {from} to lane {to} for classes {classes}")]
    NoRoute {
        /// Origin lane.
        from: LaneId,
        /// Destination lane.
        to: LaneId,
        /// The classes that were asked for.
        classes: String,
    },
    /// An actor id that the model does not know.
    #[error("actor {actor} is not active in this mobility model")]
    NoSuchActor {
        /// The offending id.
        actor: ActorId,
    },
    /// A parameter value the model cannot satisfy.
    #[error("parameter {name} = {value}: {why}")]
    InvalidParameter {
        /// Parameter name, as the card spells it.
        name: &'static str,
        /// The value that was offered.
        value: String,
        /// Why it cannot be used.
        why: &'static str,
    },
    /// The world has nothing this model can run on (no drivable lane, no junction).
    #[error("the world has no {what}")]
    EmptyWorld {
        /// What was missing.
        what: &'static str,
    },
    /// A model card that does not satisfy [`v2xw_core::card::ModelCard::validate`].
    #[error("model card: {0}")]
    Card(#[from] v2xw_core::card::CardError),
    /// The world model refused geometry this crate built (the synthetic worlds of
    /// [`crate::worlds`]).
    ///
    /// Carried as its message rather than as the error itself, because
    /// [`v2xw_world::WorldError`] is neither `Clone` nor `PartialEq` and this type is
    /// both — which is what lets a test assert on a refusal.
    #[error("world: {0}")]
    World(String),
}

/// The crate result alias.
pub type Result<T> = core::result::Result<T, MobError>;

impl From<v2xw_world::WorldError> for MobError {
    fn from(e: v2xw_world::WorldError) -> Self {
        MobError::World(e.to_string())
    }
}
