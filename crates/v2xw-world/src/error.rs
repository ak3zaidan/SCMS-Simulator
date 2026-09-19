//! The crate's error type.
//!
//! Every fallible entry point in `v2xw-world` returns [`WorldError`]. Geometry and
//! structural problems are separate variants rather than one string, because the
//! importer stage (04-models.md §1.2) reports them per object and the conformance kit
//! matches on them.

use v2xw_core::ids::{EdgeId, JunctionId, LaneId};

/// Everything that can go wrong building, validating, reading or writing a world.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorldError {
    /// A lane centreline had fewer than the two points a polyline needs.
    #[error("lane {lane} centreline has {points} point(s); a centreline needs at least 2")]
    ShortCentreline {
        /// The offending lane.
        lane: LaneId,
        /// How many points it had.
        points: usize,
    },

    /// Two successive centreline points were closer than 1 mm.
    ///
    /// The `vwp-world/1` payload requires successive points to differ by at least 1 mm
    /// (docs/protocol/vwp-v1.md §4.3), and a zero-length segment has no heading, so the
    /// model rejects it at construction rather than at export.
    #[error(
        "lane {lane} centreline points {index} and {next} are {distance_m} m apart; \
         successive points must differ by at least 1 mm (vwp-v1 §4.3)"
    )]
    DegenerateSegment {
        /// The offending lane.
        lane: LaneId,
        /// Index of the first point of the pair.
        index: usize,
        /// Index of the second point of the pair.
        next: usize,
        /// Their separation, metres.
        distance_m: f64,
    },

    /// A coordinate was not finite.
    #[error("{what} has a non-finite coordinate")]
    NonFinite {
        /// What held the bad value.
        what: String,
    },

    /// Ids must be dense: the object at index `i` must have id `i`.
    #[error("{kind} ids are not dense: index {index} holds id {found}")]
    NonDenseIds {
        /// Which collection.
        kind: &'static str,
        /// The index that failed.
        index: u32,
        /// The id found there.
        found: u32,
    },

    /// A reference pointed at an object that does not exist.
    #[error("{what} references {kind} {id}, which does not exist")]
    DanglingReference {
        /// The referring object.
        what: String,
        /// The kind of the referenced object.
        kind: &'static str,
        /// The missing id.
        id: u32,
    },

    /// A lane's cumulative arc-length table disagreed with its centreline.
    #[error(
        "lane {lane} cumulative[{index}] = {found} m but its centreline gives {expected} m \
         (tolerance {tolerance_m} m)"
    )]
    InconsistentArcLength {
        /// The offending lane.
        lane: LaneId,
        /// Which entry.
        index: usize,
        /// The stored value.
        found: f64,
        /// The value recomputed from the centreline.
        expected: f64,
        /// The tolerance applied.
        tolerance_m: f64,
    },

    /// A polygon ring had too few points to bound an area.
    #[error("{what} ring has {points} point(s); a ring needs at least 3 distinct points")]
    ShortRing {
        /// What held the ring.
        what: String,
        /// How many points it had.
        points: usize,
    },

    /// An edge listed no lanes, or a junction no internal lanes where it needs them.
    #[error("edge {edge} lists no lanes")]
    EmptyEdge {
        /// The offending edge.
        edge: EdgeId,
    },

    /// A junction's conflict matrix did not match its internal-lane count.
    #[error("junction {junction} has {internal} internal lane(s) but a {rows}-row conflict matrix")]
    ConflictMatrixShape {
        /// The offending junction.
        junction: JunctionId,
        /// How many internal lanes it has.
        internal: usize,
        /// How many rows the matrix has.
        rows: usize,
    },

    /// Connections were not in the order [`crate::model::RoadNetwork`] requires.
    ///
    /// `successors` hands out a subslice of one shared vector, so the connections must be
    /// sorted by `(from_lane, to_lane, via)`. [`crate::model::RoadNetwork::new`] sorts
    /// them; a deserialised world that is not sorted is rejected.
    #[error("connections are not sorted by (from_lane, to_lane, via) at index {index}")]
    UnsortedConnections {
        /// The first index that is out of order.
        index: usize,
    },

    /// A signal plan was not self-consistent.
    #[error("signal plan {plan}: {problem}")]
    BadSignalPlan {
        /// The plan's id, as an integer.
        plan: u32,
        /// What is wrong with it.
        problem: String,
    },

    /// A generator or importer was handed parameters it cannot satisfy.
    #[error("invalid parameter {parameter}: {problem}")]
    InvalidParameter {
        /// The parameter name, as the scenario spells it.
        parameter: String,
        /// Why it was rejected.
        problem: String,
    },

    /// No world source is registered under that id.
    #[error("unknown world source {id:?}")]
    UnknownSource {
        /// The id that was asked for.
        id: String,
    },

    /// A [`crate::WorldSource`] was handed a source specification it does not implement.
    ///
    /// The model id field is called `model` rather than `source` because `thiserror`
    /// reserves that name for an error's cause.
    #[error("world source {model} does not support {spec}")]
    UnsupportedSource {
        /// The model id of the source.
        model: String,
        /// The specification it was given.
        spec: String,
    },

    /// A serialised world could not be read.
    #[error("malformed world file at byte {offset}: {problem}")]
    Malformed {
        /// Byte offset the reader had reached.
        offset: usize,
        /// What was wrong.
        problem: String,
    },

    /// A value could not be represented in an export format.
    #[error("{what} does not fit the {format} format: {problem}")]
    Unrepresentable {
        /// The value's name.
        what: String,
        /// The target format.
        format: &'static str,
        /// Why it does not fit.
        problem: String,
    },

    /// A world failed a documented invariant of 03-interfaces.md §2.
    #[error("invariant {invariant}: {problem}")]
    Invariant {
        /// The invariant's label, e.g. `I-W2`.
        invariant: &'static str,
        /// What was violated.
        problem: String,
    },

    /// JSON serialisation or deserialisation failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Reading or writing a file failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// `Result<T>` is `Result<T, WorldError>`.
pub type Result<T, E = WorldError> = core::result::Result<T, E>;
