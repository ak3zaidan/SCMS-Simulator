//! `world/source/sumo-net` and `world/source/opendrive` — the SUMO network importer
//! (04-models.md §1.1, §1.2).
//!
//! 04-models.md §1.1 records the decision: the canonical format is this project's own
//! `world-1`, and **SUMO `net.xml` is the interchange format** — the closest external
//! match, the format the co-simulation tier of §2.8 speaks, and the path OpenDRIVE takes
//! through `netconvert --opendrive-files`. This module is that path.
//!
//! # What it reads
//!
//! The elements §1.2 names, and nothing else: `<location>`, `<type>`, `<edge>` with its
//! `<lane>`s, `<junction>` with its `<request>`s, `<connection>`, `<tlLogic>` with its
//! `<phase>`s, `<prohibition>` and `<roundabout>`. Anything else in the document is
//! ignored, as an XML reader should ignore what it does not know.
//!
//! | `net.xml` | `world-1` |
//! |---|---|
//! | `<edge>` (`function` absent or `normal`) | one [`Edge`] per direction the file states; `net.xml` already has one edge per direction |
//! | `<lane index speed length width shape allow disallow>` | one [`Lane`], index 0 rightmost, classes from `allow`/`disallow` |
//! | `<edge function="internal">` chains | **one** connector [`Lane`] per movement, the chain's shapes concatenated |
//! | `<junction>` (`type` not `internal`) | one [`Junction`]; its `<request response foes cont>` becomes the [`ConflictMatrix`] |
//! | `<junction type="internal">` | folded into the connector chain that passes through it, and counted |
//! | `<connection from to fromLane toLane via dir state tl linkIndex>` | the two [`Connection`] records of 03-interfaces.md §2 |
//! | `<tlLogic>` `<phase duration state>` | one [`SignalPlan`] **per junction** the programme controls |
//! | `<prohibition prohibitor prohibited>` | a `foes` bit and a `response` bit in the matrix |
//! | `<roundabout nodes edges>` | [`JunctionControl::Roundabout`] on each of its nodes |
//! | `<edge function="crossing">` | one [`Crossing`] |
//! | `<edge function="walkingarea">` | a [`LaneKind::Sidewalk`] lane inside the junction |
//!
//! # Lossless where the formats agree, counted where they do not
//!
//! 04-models.md §1.1 states what a SUMO import loses: buildings (the format has none),
//! land use, terrain beyond lane `z`, and material classes. Everything else on that
//! table's "preserved" side is preserved here. Where the two formats genuinely disagree —
//! an internal junction our model has no place for, a traffic-light programme that spans
//! several junctions, a per-connection class restriction our [`Connection`] cannot carry —
//! the importer records a counted [`SumoAnomaly`] and says so in the report, exactly as
//! the OSM importer reports its own (295 on the Phase 1 Manhattan extract). Nothing is
//! dropped silently.
//!
//! # A turn restriction is an absence
//!
//! `net.xml` expresses a banned turn by **not writing the `<connection>`**: netconvert has
//! already applied every OSM `restriction` relation and every `--connections` directive by
//! the time it writes the network. The importer reproduces that by not creating the
//! movement, which is lossless and leaves nothing to count. What the format *does* state
//! explicitly is right of way — `<request response foes>` and `<prohibition>` — and those
//! become the conflict matrix, which is where our model keeps the same information.
//!
//! # The frame
//!
//! A `net.xml`'s coordinates are already Cartesian metres in the network's own frame, so
//! the importer does not project: it **translates**, so that the world's origin is the
//! south-west corner of [`SumoOptions::bounds_m`] when the caller asked for one, of
//! `<location convBoundary>` when it did not, and of the imported geometry as a last
//! resort ([`SumoFrameRule`], the same ladder the OSM importer uses for the same reason).
//! Asking for the same bounds twice therefore gives the same metre coordinates and the
//! same content hash.
//!
//! The geodetic anchor is a separate question, and a harder one: `<location
//! projParameter>` is a PROJ string this crate does not evaluate. When the network states
//! a projection and a geodetic `origBoundary`, its south-west corner becomes the world's
//! [`GeoOrigin`] so that positions can be reported in degrees; the metre coordinates stay
//! in SUMO's projection, which is **not** this crate's local tangent plane, and the
//! disagreement between the two grows with distance from the origin and is not quantified
//! here (UNVERIFIED). That is recorded in the provenance rather than hidden.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{EdgeId, JunctionId, LaneId, SignalId};

use crate::error::{Result, WorldError};
use crate::model::{
    ClassMask, ConflictMatrix, Connection, Crossing, CrossingId, Edge, GeoBbox, GeoOrigin,
    Junction, JunctionControl, Lane, LaneKind, RoadClass, SignalPlan, Terrain, TurnDirection,
};
use crate::quant::{Q_POSITION_M, Q_TIME_S, quantise};

/// The model id of the `net.xml` importer (04-models.md §1.2).
pub const MODEL_ID: &str = "world/source/sumo-net";

/// The model id of the OpenDRIVE importer, which delegates to `netconvert`
/// (04-models.md §1.2).
pub const OPENDRIVE_MODEL_ID: &str = "world/source/opendrive";

/// The version both importers' cards report.
pub const MODEL_VERSION: &str = "1.0.0";

// ---------------------------------------------------------------------------
// Anomalies and the report
// ---------------------------------------------------------------------------

/// Something the importer found in the network that it could not carry across losslessly,
/// or had to decide for itself.
///
/// Every variant is counted in [`SumoImportReport::anomalies`] with a few example element
/// ids rather than raised as an error: a network that uses a junction type our model
/// approximates is not a broken network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SumoAnomaly {
    /// An `<edge>` named a `from` or `to` junction the file does not contain.
    MissingJunction,
    /// An `<edge>` had no `<lane>` children at all.
    EdgeWithoutLanes,
    /// A `<lane>` carried no usable shape, and its edge had none to fall back on.
    LaneWithoutShape,
    /// A `<lane>` shape had fewer than two distinct points once duplicates were removed.
    LaneShapeTooShort,
    /// A `<lane>` had no `speed`, and neither its `<type>` nor the options supplied one.
    SpeedDefaulted,
    /// A `<lane>` had no `width`, so the option's default was used.
    WidthDefaulted,
    /// A `<lane>` stated both `allow` and `disallow`; `allow` was taken.
    AllowAndDisallow,
    /// A `<lane>`'s stated `length` disagreed with the length of its own shape by more
    /// than a centimetre per metre.
    LengthDisagreesWithShape,
    /// A `<connection>` named an edge or a lane the file does not contain.
    DanglingConnection,
    /// A `<connection>` had no `dir`, so the turn was classified from the geometry.
    TurnDirectionInferred,
    /// A `<connection>`'s `dir` was not one of the documented codes.
    UnknownTurnDirection,
    /// A `<connection>`'s `state` was not one of the documented codes.
    UnknownLinkState,
    /// A `<connection>` restricted which classes may use it, which a
    /// [`Connection`](crate::model::Connection) cannot carry.
    ConnectionAccessDropped,
    /// A `<connection>` allowed no class at all, and was marked not permitted.
    ConnectionAllowsNothing,
    /// A movement's `via` chain passed through an internal junction; the chain's lanes
    /// were concatenated into one connector and the internal junction was dropped.
    InternalJunctionCollapsed,
    /// A `via` lane id did not resolve to an internal lane.
    DanglingVia,
    /// A movement had no `via` lane, so it has no connector and no row in the conflict
    /// matrix.
    MovementWithoutInternalLane,
    /// An internal lane belonged to no junction the file lists, and was attached to the
    /// junction whose id is the longest prefix of its own.
    InternalLaneAttachedByName,
    /// An internal lane could not be attached to any junction at all.
    OrphanInternalLane,
    /// A `<junction>`'s `<request>` count did not match its controlled-movement count, so
    /// the conflict matrix was computed from the geometry instead of read from the file.
    RequestCountMismatch,
    /// A `<request>` bitstring was shorter than the number of links.
    RequestStringTooShort,
    /// A `<request>` marked a link as something to respond to without also marking it a
    /// foe, which the format is not supposed to do.
    ResponseWithoutFoe,
    /// A `<junction type>` has no exact equivalent in
    /// [`JunctionControl`](crate::model::JunctionControl) and was approximated.
    JunctionTypeApproximated,
    /// A `<junction type>` was not one of the documented values.
    UnknownJunctionType,
    /// A junction's `shape` was replaced by the convex hull of its own lane ends, because
    /// the model's junction area is convex and must contain the junction's position.
    JunctionShapeHulled,
    /// A `<tlLogic>` controlled links at more than one junction, so it became one plan per
    /// junction.
    TlsSplitAcrossJunctions,
    /// A `<tlLogic>` had a second `programID`, which was ignored.
    ExtraSignalProgramme,
    /// A `<phase>` had a duration of zero or less and was dropped.
    ZeroDurationPhase,
    /// A `<phase>`'s `state` string was shorter than the highest link index it had to
    /// cover, so the programme was not imported.
    SignalStateTooShort,
    /// A `<phase>`'s `state` carried a letter that is not one of the documented ones.
    UnknownSignalState,
    /// A junction of a traffic-light type had no programme, so it fell back to priority.
    SignalisedWithoutProgramme,
    /// A `<prohibition>` could not be resolved to two movements at one junction.
    ProhibitionUnmatched,
    /// A `<roundabout>` named a node the file does not contain.
    RoundaboutNodeUnknown,
    /// An `<edge function="crossing">` could not be attached to a junction.
    CrossingWithoutJunction,
    /// An `<edge function="walkingarea">` could not be attached to a junction.
    WalkingAreaWithoutJunction,
    /// An element was dropped because it lay outside the requested bounds.
    OutsideBounds,
    /// A lane centreline crossed itself, which the world model forbids, so the lane was
    /// dropped.
    SelfIntersectingLane,
    /// The network declared a projection this crate cannot evaluate, so the geodetic
    /// origin came from `origBoundary` and the metre frame stayed in SUMO's projection.
    ProjectionNotEvaluated,
    /// The network declared no geodetic information at all, so the world's origin is null
    /// island.
    NoGeodeticAnchor,
}

impl SumoAnomaly {
    /// Every category, in report order.
    pub const ALL: [SumoAnomaly; 39] = [
        SumoAnomaly::MissingJunction,
        SumoAnomaly::EdgeWithoutLanes,
        SumoAnomaly::LaneWithoutShape,
        SumoAnomaly::LaneShapeTooShort,
        SumoAnomaly::SpeedDefaulted,
        SumoAnomaly::WidthDefaulted,
        SumoAnomaly::AllowAndDisallow,
        SumoAnomaly::LengthDisagreesWithShape,
        SumoAnomaly::DanglingConnection,
        SumoAnomaly::TurnDirectionInferred,
        SumoAnomaly::UnknownTurnDirection,
        SumoAnomaly::UnknownLinkState,
        SumoAnomaly::ConnectionAccessDropped,
        SumoAnomaly::ConnectionAllowsNothing,
        SumoAnomaly::InternalJunctionCollapsed,
        SumoAnomaly::DanglingVia,
        SumoAnomaly::MovementWithoutInternalLane,
        SumoAnomaly::InternalLaneAttachedByName,
        SumoAnomaly::OrphanInternalLane,
        SumoAnomaly::RequestCountMismatch,
        SumoAnomaly::RequestStringTooShort,
        SumoAnomaly::ResponseWithoutFoe,
        SumoAnomaly::JunctionTypeApproximated,
        SumoAnomaly::UnknownJunctionType,
        SumoAnomaly::JunctionShapeHulled,
        SumoAnomaly::TlsSplitAcrossJunctions,
        SumoAnomaly::ExtraSignalProgramme,
        SumoAnomaly::ZeroDurationPhase,
        SumoAnomaly::SignalStateTooShort,
        SumoAnomaly::UnknownSignalState,
        SumoAnomaly::SignalisedWithoutProgramme,
        SumoAnomaly::ProhibitionUnmatched,
        SumoAnomaly::RoundaboutNodeUnknown,
        SumoAnomaly::CrossingWithoutJunction,
        SumoAnomaly::WalkingAreaWithoutJunction,
        SumoAnomaly::OutsideBounds,
        SumoAnomaly::SelfIntersectingLane,
        SumoAnomaly::ProjectionNotEvaluated,
        SumoAnomaly::NoGeodeticAnchor,
    ];

    /// A stable kebab-case label for the report.
    pub const fn label(self) -> &'static str {
        match self {
            SumoAnomaly::MissingJunction => "missing-junction",
            SumoAnomaly::EdgeWithoutLanes => "edge-without-lanes",
            SumoAnomaly::LaneWithoutShape => "lane-without-shape",
            SumoAnomaly::LaneShapeTooShort => "lane-shape-too-short",
            SumoAnomaly::SpeedDefaulted => "speed-defaulted",
            SumoAnomaly::WidthDefaulted => "width-defaulted",
            SumoAnomaly::AllowAndDisallow => "allow-and-disallow",
            SumoAnomaly::LengthDisagreesWithShape => "length-disagrees-with-shape",
            SumoAnomaly::DanglingConnection => "dangling-connection",
            SumoAnomaly::TurnDirectionInferred => "turn-direction-inferred",
            SumoAnomaly::UnknownTurnDirection => "unknown-turn-direction",
            SumoAnomaly::UnknownLinkState => "unknown-link-state",
            SumoAnomaly::ConnectionAccessDropped => "connection-access-dropped",
            SumoAnomaly::ConnectionAllowsNothing => "connection-allows-nothing",
            SumoAnomaly::InternalJunctionCollapsed => "internal-junction-collapsed",
            SumoAnomaly::DanglingVia => "dangling-via",
            SumoAnomaly::MovementWithoutInternalLane => "movement-without-internal-lane",
            SumoAnomaly::InternalLaneAttachedByName => "internal-lane-attached-by-name",
            SumoAnomaly::OrphanInternalLane => "orphan-internal-lane",
            SumoAnomaly::RequestCountMismatch => "request-count-mismatch",
            SumoAnomaly::RequestStringTooShort => "request-string-too-short",
            SumoAnomaly::ResponseWithoutFoe => "response-without-foe",
            SumoAnomaly::JunctionTypeApproximated => "junction-type-approximated",
            SumoAnomaly::UnknownJunctionType => "unknown-junction-type",
            SumoAnomaly::JunctionShapeHulled => "junction-shape-hulled",
            SumoAnomaly::TlsSplitAcrossJunctions => "tls-split-across-junctions",
            SumoAnomaly::ExtraSignalProgramme => "extra-signal-programme",
            SumoAnomaly::ZeroDurationPhase => "zero-duration-phase",
            SumoAnomaly::SignalStateTooShort => "signal-state-too-short",
            SumoAnomaly::UnknownSignalState => "unknown-signal-state",
            SumoAnomaly::SignalisedWithoutProgramme => "signalised-without-programme",
            SumoAnomaly::ProhibitionUnmatched => "prohibition-unmatched",
            SumoAnomaly::RoundaboutNodeUnknown => "roundabout-node-unknown",
            SumoAnomaly::CrossingWithoutJunction => "crossing-without-junction",
            SumoAnomaly::WalkingAreaWithoutJunction => "walking-area-without-junction",
            SumoAnomaly::OutsideBounds => "outside-bounds",
            SumoAnomaly::SelfIntersectingLane => "self-intersecting-lane",
            SumoAnomaly::ProjectionNotEvaluated => "projection-not-evaluated",
            SumoAnomaly::NoGeodeticAnchor => "no-geodetic-anchor",
        }
    }
}

impl core::fmt::Display for SumoAnomaly {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// How many example element ids the report keeps per anomaly category.
pub const ANOMALY_SAMPLES: usize = 4;

/// What fixed the world's metre frame.
///
/// The same ladder the OSM importer climbs, and for the same reason: the frame is the one
/// thing every coordinate depends on, so it must not depend on which edge happens to
/// stick out furthest.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SumoFrameRule {
    /// The south-west corner of [`SumoOptions::bounds_m`]: the caller's own request, and
    /// the only choice that does not depend on the network at all.
    RequestedBounds,
    /// The south-west corner of `<location convBoundary>`, which netconvert writes with
    /// the network's own extent.
    ConvBoundary,
    /// The south-west corner of the imported geometry. The last resort, for a network
    /// with no `<location>` imported with no bounds.
    #[default]
    ImportedGeometry,
}

impl SumoFrameRule {
    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            SumoFrameRule::RequestedBounds => "requested-bounds",
            SumoFrameRule::ConvBoundary => "conv-boundary",
            SumoFrameRule::ImportedGeometry => "imported-geometry",
        }
    }

    /// The sentence the provenance records beside the origin.
    pub const fn rule(self) -> &'static str {
        match self {
            SumoFrameRule::RequestedBounds => {
                "south-west corner of the requested bounds (D6): independent of what the \
                 network happens to contain"
            }
            SumoFrameRule::ConvBoundary => {
                "south-west corner of <location convBoundary> (D6): no bounds were \
                 requested"
            }
            SumoFrameRule::ImportedGeometry => {
                "south-west corner of the imported geometry (D6): no bounds were requested \
                 and the network declares no <location>, so the frame moves if the \
                 geometry does"
            }
        }
    }
}

/// The tallies of one import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SumoCounts {
    /// `<edge>` elements in the file, of every function.
    pub sumo_edges: u64,
    /// `<edge>` elements with `function="internal"`.
    pub sumo_internal_edges: u64,
    /// `<lane>` elements in the file.
    pub sumo_lanes: u64,
    /// `<junction>` elements in the file, internal ones included.
    pub sumo_junctions: u64,
    /// `<junction type="internal">` elements, which our model folds away.
    pub sumo_internal_junctions: u64,
    /// `<connection>` elements in the file, the internal-origin halves included.
    pub sumo_connections: u64,
    /// `<tlLogic>` elements in the file.
    pub sumo_tl_logics: u64,
    /// `<prohibition>` elements in the file.
    pub sumo_prohibitions: u64,
    /// `<roundabout>` elements in the file.
    pub sumo_roundabouts: u64,
    /// Edges in the world, including each junction's synthetic internal edge.
    pub edges: u64,
    /// Lanes in the world, of every kind.
    pub lanes: u64,
    /// Lanes a motor vehicle may drive on.
    pub drivable_lanes: u64,
    /// Junction connector lanes.
    pub internal_lanes: u64,
    /// Sidewalk lanes, which is what a walking area becomes.
    pub sidewalk_lanes: u64,
    /// Junctions in the world.
    pub junctions: u64,
    /// Junctions with three or more arms.
    pub major_junctions: u64,
    /// Junctions carrying a signal plan.
    pub signalised_junctions: u64,
    /// Connections in the world (each movement appears twice: see
    /// [`Connection`](crate::model::Connection)).
    pub connections: u64,
    /// Movements the importer built: half the connection count that has a connector.
    pub movements: u64,
    /// Movements marked not permitted.
    pub banned_movements: u64,
    /// Signal plans in the world.
    pub signal_plans: u64,
    /// Crossings in the world.
    pub crossings: u64,
    /// Conflict matrices read from `<request>` elements.
    pub matrices_from_requests: u64,
    /// Conflict matrices computed from the geometry because the file's did not match.
    pub matrices_from_geometry: u64,
    /// `<prohibition>` elements applied to a matrix.
    pub prohibitions_applied: u64,
    /// Lane speed limits that came from the lane's own `speed`.
    pub speeds_from_lane: u64,
    /// Lane speed limits that came from the edge's `<type>`.
    pub speeds_from_type: u64,
    /// Lane widths that came from the lane's own `width`.
    pub widths_from_lane: u64,
}

/// What the source network contained and what the importer made of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SumoImportReport {
    /// What was imported: the file path, or the caller's label for a byte slice.
    pub source_id: String,
    /// SHA-256 of the source bytes, lower-case hex.
    pub source_sha256: String,
    /// Size of the source, bytes.
    pub source_bytes: u64,
    /// The `version` attribute of the `<net>` root, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_version: Option<String>,
    /// The `<location>` element, as read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SumoLocation>,
    /// What fixed the metre frame.
    #[serde(default)]
    pub frame: SumoFrameRule,
    /// What was added to every source coordinate, metres.
    pub shift_m: (f64, f64),
    /// The bounds the caller asked for, in source coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_bounds_m: Option<[f64; 4]>,
    /// The world's own extent, `(east, north)` metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent_m: Option<(f64, f64)>,
    /// The geodetic box the world covers, when the network carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geodetic_bbox: Option<GeoBbox>,
    /// What the file held and what came out.
    pub counts: SumoCounts,
    /// Every anomaly category that fired, with its count.
    pub anomalies: BTreeMap<SumoAnomaly, u64>,
    /// Up to [`ANOMALY_SAMPLES`] example element ids per anomaly, for a human.
    pub samples: BTreeMap<SumoAnomaly, Vec<String>>,
    /// The `netconvert` conversion log, when the world came through it
    /// (04-models.md §1.1 requires it in the provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netconvert_log: Option<String>,
}

impl SumoImportReport {
    /// How many times `kind` fired.
    pub fn anomaly(&self, kind: SumoAnomaly) -> u64 {
        self.anomalies.get(&kind).copied().unwrap_or(0)
    }

    /// Every anomaly, of every category, added up.
    pub fn total_anomalies(&self) -> u64 {
        self.anomalies.values().sum()
    }

    /// Records one anomaly against `element`, an element id (empty when there is none).
    fn note(&mut self, kind: SumoAnomaly, element: &str) {
        *self.anomalies.entry(kind).or_insert(0) += 1;
        let samples = self.samples.entry(kind).or_default();
        if samples.len() < ANOMALY_SAMPLES
            && !element.is_empty()
            && !samples.iter().any(|s| s == element)
        {
            samples.push(element.to_string());
        }
    }

    /// The report as a block of text, for a log or a console.
    pub fn to_text(&self) -> String {
        use core::fmt::Write as _;
        let mut s = String::new();
        let c = &self.counts;
        let _ = writeln!(s, "source            {}", self.source_id);
        let _ = writeln!(
            s,
            "sha256            {} ({} bytes)",
            self.source_sha256, self.source_bytes
        );
        let _ = writeln!(
            s,
            "net               version {}",
            self.net_version.as_deref().unwrap_or("(unstated)")
        );
        let _ = writeln!(
            s,
            "frame             origin from the {}, shifted by ({:.3}, {:.3}) m",
            self.frame.label(),
            self.shift_m.0,
            self.shift_m.1
        );
        if let Some((east, north)) = self.extent_m {
            let _ = writeln!(
                s,
                "extent            {east:.0} x {north:.0} m = {:.2} km^2",
                east * north / 1e6
            );
        }
        if let Some(b) = self.geodetic_bbox {
            let _ = writeln!(
                s,
                "geodetic          {:.6},{:.6} .. {:.6},{:.6}",
                b.min_lon_deg, b.min_lat_deg, b.max_lon_deg, b.max_lat_deg
            );
        }
        let _ = writeln!(
            s,
            "sumo              {} edges ({} internal), {} lanes, {} junctions ({} internal)",
            c.sumo_edges,
            c.sumo_internal_edges,
            c.sumo_lanes,
            c.sumo_junctions,
            c.sumo_internal_junctions
        );
        let _ = writeln!(
            s,
            "sumo              {} connections, {} tlLogics, {} prohibitions, {} roundabouts",
            c.sumo_connections, c.sumo_tl_logics, c.sumo_prohibitions, c.sumo_roundabouts
        );
        let _ = writeln!(
            s,
            "network           {} junctions ({} with 3+ arms, {} signalised), {} edges, \
             {} lanes",
            c.junctions, c.major_junctions, c.signalised_junctions, c.edges, c.lanes
        );
        let _ = writeln!(
            s,
            "lanes             {} drivable, {} internal, {} sidewalk",
            c.drivable_lanes, c.internal_lanes, c.sidewalk_lanes
        );
        let _ = writeln!(
            s,
            "movements         {} ({} banned), {} connection records",
            c.movements, c.banned_movements, c.connections
        );
        let _ = writeln!(
            s,
            "right of way      {} matrices from <request>, {} from geometry, {} \
             prohibitions applied",
            c.matrices_from_requests, c.matrices_from_geometry, c.prohibitions_applied
        );
        let _ = writeln!(
            s,
            "attributes        speeds {} from lane / {} from type, widths {} from lane",
            c.speeds_from_lane, c.speeds_from_type, c.widths_from_lane
        );
        let _ = writeln!(
            s,
            "other             {} signal plans, {} crossings",
            c.signal_plans, c.crossings
        );
        let _ = writeln!(s, "anomalies         {} in total", self.total_anomalies());
        for (kind, count) in &self.anomalies {
            let samples = self
                .samples
                .get(kind)
                .map(|ids| ids.join(", "))
                .unwrap_or_default();
            if samples.is_empty() {
                let _ = writeln!(s, "  {:<30} {}", kind.label(), count);
            } else {
                let _ = writeln!(s, "  {:<30} {:<8} e.g. {}", kind.label(), count, samples);
            }
        }
        if let Some(log) = &self.netconvert_log {
            let _ = writeln!(s, "netconvert        {} bytes of log", log.len());
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Everything [`import_sumo_net`] reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SumoOptions {
    /// The common import options: import date, the index sizing and the default
    /// environment class.
    pub import: crate::ImportOptions,
    /// Keep only geometry inside this box, in the **source network's** coordinates,
    /// `[min_x, min_y, max_x, max_y]`.
    ///
    /// The box also fixes the frame: the world's origin is its south-west corner, so
    /// asking for the same box twice gives the same metre coordinates
    /// ([`SumoFrameRule::RequestedBounds`]).
    ///
    /// Filtering is whole-element: an edge is kept when any of its lane points is inside
    /// the box, and is then kept entire. Geometry is **not** cut at the boundary — the OSM
    /// importer's clip is not implemented here — so the extent can exceed the request by
    /// one edge length, and every dropped element is counted as
    /// [`SumoAnomaly::OutsideBounds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_m: Option<[f64; 4]>,
    /// Import walking areas and crossings as pedestrian geometry.
    pub pedestrian_layer: bool,
    /// The lane width to use, metres, when a `<lane>` states none.
    ///
    /// **`TODO: calibrate`**: netconvert omits `width` when it equals its own default, so
    /// this value stands in for that default on most networks, and the default belongs to
    /// whichever SUMO version wrote the file. 3.2 m is SUMO's documented lane-width
    /// constant as recalled rather than as read from a cached document, which is why the
    /// model card marks it and why every lane that takes it is counted
    /// ([`SumoAnomaly::WidthDefaulted`]).
    pub default_lane_width_m: f64,
    /// The speed limit to use, m/s, when neither the `<lane>` nor its `<type>` states one.
    ///
    /// **`TODO: calibrate`**, exactly as the OSM importer's class defaults are: a fallback
    /// speed limit is a statement about a jurisdiction. Every lane that takes it is
    /// counted ([`SumoAnomaly::SpeedDefaulted`]).
    pub default_speed_mps: f64,
    /// The licence recorded for the road layer.
    ///
    /// `net.xml` carries no licence of its own (04-models.md §1.1: "the format has no
    /// separate license"), and the *data*'s licence depends on where the network came
    /// from: ODbL for an OSM import, something else for a hand-drawn network. The default
    /// says so rather than claiming one.
    pub roads_licence: String,
    /// The attribution string the road layer requires, when the caller knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roads_attribution: Option<String>,
    /// Height of a signal lantern above the lane it faces, metres.
    ///
    /// **`TODO: calibrate`**: `net.xml` states no mounting height, and the value affects
    /// rendering and the RSU-to-signal line of sight and nothing else. It is the same
    /// placeholder, with the same calibration plan, as the grid generator's.
    pub signal_head_height_m: f64,
    /// The terrain grid to attach. `None` leaves the world's ground at the lane `z` the
    /// network carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<Terrain>,
}

/// The licence string an import records when the caller does not state one.
pub const UNSTATED_LICENCE: &str = "UNSTATED";

impl Default for SumoOptions {
    fn default() -> Self {
        Self {
            import: crate::ImportOptions::default(),
            bounds_m: None,
            pedestrian_layer: true,
            default_lane_width_m: 3.2,
            default_speed_mps: 13.89,
            roads_licence: UNSTATED_LICENCE.to_string(),
            roads_attribution: None,
            signal_head_height_m: 5.0,
            terrain: None,
        }
    }
}

impl SumoOptions {
    /// The options with an import date, which no part of the engine may read from a clock.
    #[must_use]
    pub fn imported_at(mut self, when: impl Into<String>) -> Self {
        self.import.imported_at = when.into();
        self
    }

    /// The options restricted to a box in the source network's coordinates.
    #[must_use]
    pub fn bounds_m(mut self, bounds: [f64; 4]) -> Self {
        self.bounds_m = Some(bounds);
        self
    }

    /// The options with the road layer's licence stated.
    #[must_use]
    pub fn roads_licence(mut self, licence: impl Into<String>) -> Self {
        self.roads_licence = licence.into();
        self
    }

    /// Checks that the options describe an import that can succeed.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter.
    pub fn validate(&self) -> Result<()> {
        let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
            parameter: parameter.to_string(),
            problem,
        };
        if !(self.default_lane_width_m.is_finite() && self.default_lane_width_m > 0.0) {
            return Err(bad(
                "default_lane_width_m",
                format!("{} m is not a positive width", self.default_lane_width_m),
            ));
        }
        if !(self.default_speed_mps.is_finite() && self.default_speed_mps > 0.0) {
            return Err(bad(
                "default_speed_mps",
                format!("{} m/s is not a positive speed", self.default_speed_mps),
            ));
        }
        if let Some([min_x, min_y, max_x, max_y]) = self.bounds_m {
            for (name, v) in [
                ("bounds_m[0]", min_x),
                ("bounds_m[1]", min_y),
                ("bounds_m[2]", max_x),
                ("bounds_m[3]", max_y),
            ] {
                if !v.is_finite() {
                    return Err(bad(name, format!("{v} is not finite")));
                }
            }
            if max_x < min_x || max_y < min_y {
                return Err(bad(
                    "bounds_m",
                    format!("({min_x}, {min_y}) .. ({max_x}, {max_y}) is inverted"),
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The raw file
// ---------------------------------------------------------------------------

/// The `<location>` element: how the network's coordinates relate to the world's.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SumoLocation {
    /// `netOffset`: what netconvert added to the original coordinates to get these.
    pub net_offset: (f64, f64),
    /// `convBoundary`: the network's extent in its own coordinates,
    /// `[min_x, min_y, max_x, max_y]`.
    pub conv_boundary: [f64; 4],
    /// `origBoundary`: the same extent in the original coordinates, which for an
    /// OSM-derived network is degrees of longitude and latitude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_boundary: Option<[f64; 4]>,
    /// Whether `projParameter` was anything other than `!` (no projection).
    pub projected: bool,
}

impl SumoLocation {
    /// The original boundary read as a geodetic box, when it plausibly is one.
    ///
    /// A projected network's `origBoundary` is degrees; an unprojected one's is metres,
    /// and a metre box the size of a city is not a plausible degree box. The test is
    /// therefore "inside the valid range of longitude and latitude", which is the only
    /// test available without evaluating `projParameter`.
    pub fn geodetic_boundary(&self) -> Option<GeoBbox> {
        let [min_lon, min_lat, max_lon, max_lat] = self.orig_boundary?;
        let plausible = (-180.0..=180.0).contains(&min_lon)
            && (-180.0..=180.0).contains(&max_lon)
            && (-90.0..=90.0).contains(&min_lat)
            && (-90.0..=90.0).contains(&max_lat)
            && max_lon >= min_lon
            && max_lat >= min_lat;
        plausible.then(|| GeoBbox::new(min_lat, min_lon, max_lat, max_lon))
    }
}

/// What an `<edge>`'s `function` attribute says it is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EdgeFunction {
    /// A street between two junctions: `function` absent, or `normal`.
    #[default]
    Normal,
    /// A connector inside a junction: `function="internal"`.
    Internal,
    /// A pedestrian crossing: `function="crossing"`.
    Crossing,
    /// The pedestrian area of a junction: `function="walkingarea"`.
    WalkingArea,
    /// A connector edge between two networks: `function="connector"`.
    Connector,
    /// Anything else the attribute said.
    Other,
}

impl EdgeFunction {
    /// The function an attribute value names.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            None | Some("normal") => EdgeFunction::Normal,
            Some("internal") => EdgeFunction::Internal,
            Some("crossing") => EdgeFunction::Crossing,
            Some("walkingarea") => EdgeFunction::WalkingArea,
            Some("connector") => EdgeFunction::Connector,
            Some(_) => EdgeFunction::Other,
        }
    }

    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            EdgeFunction::Normal => "normal",
            EdgeFunction::Internal => "internal",
            EdgeFunction::Crossing => "crossing",
            EdgeFunction::WalkingArea => "walkingarea",
            EdgeFunction::Connector => "connector",
            EdgeFunction::Other => "other",
        }
    }
}

/// One `<lane>`, as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawLane {
    /// The lane's own id, `edgeId_index`.
    pub id: String,
    /// `index`: 0 is the rightmost.
    pub index: u32,
    /// `speed`, m/s.
    pub speed_mps: Option<f64>,
    /// `length`, metres, as the file states it.
    pub length_m: Option<f64>,
    /// `width`, metres.
    pub width_m: Option<f64>,
    /// `shape`, the lane's own centreline.
    pub shape: Vec<Vec3>,
    /// `allow`: the vClasses permitted here.
    pub allow: Option<String>,
    /// `disallow`: the vClasses forbidden here.
    pub disallow: Option<String>,
}

/// One `<edge>`, as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEdge {
    /// The edge's id. An internal edge's starts with `:`.
    pub id: String,
    /// `from`, the junction it leaves.
    pub from: Option<String>,
    /// `to`, the junction it enters.
    pub to: Option<String>,
    /// `function`.
    pub function: EdgeFunction,
    /// `type`, the edge type whose defaults apply.
    pub edge_type: Option<String>,
    /// `priority`.
    pub priority: Option<i64>,
    /// `shape`, when the edge states one.
    pub shape: Vec<Vec3>,
    /// Its lanes, in file order.
    pub lanes: Vec<RawLane>,
}

/// One `<request>` of a junction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRequest {
    /// `index`: the link index this row is about.
    pub index: u32,
    /// `response`: the links this one gives way to, as a bitstring.
    pub response: String,
    /// `foes`: the links this one conflicts with, as a bitstring.
    pub foes: String,
    /// `cont`: whether the link has an internal waiting position.
    pub cont: bool,
}

impl RawRequest {
    /// Whether bit `link` of a SUMO bitstring is set.
    ///
    /// **SUMO writes these strings with the highest link index first**, so the character
    /// for link 0 is the *last* one. Reading them left to right is the single easiest way
    /// to import a junction's right of way exactly backwards, which is why this is a named
    /// function with a test rather than an index expression.
    pub fn bit(bits: &str, link: usize) -> Option<bool> {
        let n = bits.chars().count();
        if link >= n {
            return None;
        }
        bits.chars().nth(n - 1 - link).map(|c| c == '1')
    }

    /// Whether this link gives way to link `other`.
    pub fn responds_to(&self, other: usize) -> Option<bool> {
        RawRequest::bit(&self.response, other)
    }

    /// Whether this link conflicts with link `other`.
    pub fn foe_of(&self, other: usize) -> Option<bool> {
        RawRequest::bit(&self.foes, other)
    }
}

/// One `<junction>`, as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawJunction {
    /// The junction's id.
    pub id: String,
    /// `type`.
    pub junction_type: String,
    /// Its position.
    pub position: Vec3,
    /// `incLanes`, the incoming lane ids in link order.
    pub inc_lanes: Vec<String>,
    /// `intLanes`, the internal lane ids in link order.
    pub int_lanes: Vec<String>,
    /// `shape`, the junction area as the file draws it.
    pub shape: Vec<Vec3>,
    /// Its `<request>` rows, in file order.
    pub requests: Vec<RawRequest>,
}

/// One `<connection>`, as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawConnection {
    /// `from`, an edge id.
    pub from: String,
    /// `to`, an edge id.
    pub to: String,
    /// `fromLane`.
    pub from_lane: u32,
    /// `toLane`.
    pub to_lane: u32,
    /// `via`, an internal lane id.
    pub via: Option<String>,
    /// `dir`, the turn code.
    pub dir: Option<String>,
    /// `state`, the link state code.
    pub state: Option<String>,
    /// `tl`, the traffic light that controls it.
    pub tl: Option<String>,
    /// `linkIndex`, its index within that traffic light and within the junction's
    /// requests.
    pub link_index: Option<u32>,
    /// `allow`.
    pub allow: Option<String>,
    /// `disallow`.
    pub disallow: Option<String>,
}

/// One `<tlLogic>`, as read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTlLogic {
    /// The programme's id, which is also the controlled junction's id for a simple
    /// network.
    pub id: String,
    /// `programID`.
    pub program_id: String,
    /// `type`: `static`, `actuated`, `delay_based`, …
    pub logic_type: String,
    /// `offset`, seconds.
    pub offset_s: f64,
    /// Its phases, in file order: `(duration_s, state)`.
    pub phases: Vec<(f64, String)>,
}

/// One `<type>`, as read: the defaults an edge of that type inherits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawType {
    /// The type's id, e.g. `highway.residential`.
    pub id: String,
    /// `speed`, m/s.
    pub speed_mps: Option<f64>,
    /// `numLanes`.
    pub num_lanes: Option<u32>,
    /// `priority`.
    pub priority: Option<i64>,
    /// `allow`.
    pub allow: Option<String>,
    /// `disallow`.
    pub disallow: Option<String>,
}

/// One `<prohibition>`, as read: a right-of-way relation between two movements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawProhibition {
    /// `prohibitor`, in the form `fromEdge->toEdge`.
    pub prohibitor: String,
    /// `prohibited`, in the same form: the movement that must give way.
    pub prohibited: String,
}

/// A parsed `net.xml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SumoNet {
    /// The `version` attribute of the root element.
    pub version: Option<String>,
    /// The `<location>` element.
    pub location: Option<SumoLocation>,
    /// Every `<type>`, sorted by id.
    pub types: Vec<RawType>,
    /// Every `<edge>`, sorted by id.
    pub edges: Vec<RawEdge>,
    /// Every `<junction>`, sorted by id.
    pub junctions: Vec<RawJunction>,
    /// Every `<connection>`, in file order.
    pub connections: Vec<RawConnection>,
    /// Every `<tlLogic>`, sorted by `(id, programID)`.
    pub tl_logics: Vec<RawTlLogic>,
    /// Every `<prohibition>`, in file order.
    pub prohibitions: Vec<RawProhibition>,
    /// Every `<roundabout>`'s node ids.
    pub roundabouts: Vec<Vec<String>>,
}

impl SumoNet {
    /// The edge with that id, or `None`.
    pub fn edge(&self, id: &str) -> Option<&RawEdge> {
        self.edges
            .binary_search_by(|e| e.id.as_str().cmp(id))
            .ok()
            .map(|i| &self.edges[i])
    }

    /// The junction with that id, or `None`.
    pub fn junction(&self, id: &str) -> Option<&RawJunction> {
        self.junctions
            .binary_search_by(|j| j.id.as_str().cmp(id))
            .ok()
            .map(|i| &self.junctions[i])
    }

    /// The type with that id, or `None`.
    pub fn edge_type(&self, id: &str) -> Option<&RawType> {
        self.types
            .binary_search_by(|t| t.id.as_str().cmp(id))
            .ok()
            .map(|i| &self.types[i])
    }
}

/// The element the streaming parser is in the middle of.
enum Pending {
    Edge(RawEdge),
    Junction(RawJunction),
    TlLogic(RawTlLogic),
}

/// Parses a SUMO `net.xml` document.
///
/// One streaming pass with `quick-xml`, like the OSM importer's: no DOM. Unknown elements
/// and attributes are ignored. Elements are sorted by id afterwards, so that every later
/// stage's iteration order is independent of the file's.
///
/// # Errors
///
/// [`WorldError::Malformed`] if the XML does not parse, **or if its root element is not
/// `<net>`** — the check that stops an HTML error page, a `.rou.xml` or a plain
/// `.nod.xml` from importing as a valid, empty, content-addressed world.
pub fn parse_sumo_net(xml: &[u8]) -> Result<SumoNet> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut net = SumoNet::default();
    let mut pending: Option<Pending> = None;
    let mut buf = Vec::new();
    let mut saw_net_root = false;

    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| {
            let offset = reader.buffer_position() as usize;
            WorldError::Malformed {
                offset,
                problem: format!("sumo net.xml: {e}"),
            }
        })?;
        match event {
            quick_xml::events::Event::Eof => break,
            quick_xml::events::Event::Start(ref e) | quick_xml::events::Event::Empty(ref e) => {
                let empty = matches!(event, quick_xml::events::Event::Empty(_));
                match e.name().into_inner() {
                    "net" => {
                        saw_net_root = true;
                        net.version = attr(e, "version");
                    }
                    "location" => {
                        let conv = attr(e, "convBoundary")
                            .as_deref()
                            .and_then(parse_boundary)
                            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
                        let offset = attr(e, "netOffset")
                            .as_deref()
                            .and_then(parse_pair)
                            .unwrap_or((0.0, 0.0));
                        net.location = Some(SumoLocation {
                            net_offset: offset,
                            conv_boundary: conv,
                            orig_boundary: attr(e, "origBoundary")
                                .as_deref()
                                .and_then(parse_boundary),
                            projected: attr(e, "projParameter")
                                .map(|p| p.trim() != "!" && !p.trim().is_empty())
                                .unwrap_or(false),
                        });
                    }
                    "type" => {
                        if let Some(id) = attr(e, "id") {
                            net.types.push(RawType {
                                id,
                                speed_mps: attr_f(e, "speed"),
                                num_lanes: attr_u32(e, "numLanes"),
                                priority: attr_i64(e, "priority"),
                                allow: attr(e, "allow"),
                                disallow: attr(e, "disallow"),
                            });
                        }
                    }
                    "edge" => {
                        if let Some(id) = attr(e, "id") {
                            let edge = RawEdge {
                                id,
                                from: attr(e, "from"),
                                to: attr(e, "to"),
                                function: EdgeFunction::parse(attr(e, "function").as_deref()),
                                edge_type: attr(e, "type"),
                                priority: attr_i64(e, "priority"),
                                shape: attr(e, "shape")
                                    .as_deref()
                                    .map(parse_shape)
                                    .unwrap_or_default(),
                                lanes: Vec::new(),
                            };
                            if empty {
                                net.edges.push(edge);
                            } else {
                                pending = Some(Pending::Edge(edge));
                            }
                        }
                    }
                    "lane" => {
                        if let Some(Pending::Edge(edge)) = pending.as_mut() {
                            if let Some(id) = attr(e, "id") {
                                edge.lanes.push(RawLane {
                                    id,
                                    index: attr_u32(e, "index").unwrap_or(edge.lanes.len() as u32),
                                    speed_mps: attr_f(e, "speed"),
                                    length_m: attr_f(e, "length"),
                                    width_m: attr_f(e, "width"),
                                    shape: attr(e, "shape")
                                        .as_deref()
                                        .map(parse_shape)
                                        .unwrap_or_default(),
                                    allow: attr(e, "allow"),
                                    disallow: attr(e, "disallow"),
                                });
                            }
                        }
                    }
                    "junction" => {
                        if let Some(id) = attr(e, "id") {
                            let junction = RawJunction {
                                id,
                                junction_type: attr(e, "type").unwrap_or_default(),
                                position: Vec3::new(
                                    attr_f(e, "x").unwrap_or(0.0),
                                    attr_f(e, "y").unwrap_or(0.0),
                                    attr_f(e, "z").unwrap_or(0.0),
                                ),
                                inc_lanes: attr(e, "incLanes")
                                    .as_deref()
                                    .map(split_ids)
                                    .unwrap_or_default(),
                                int_lanes: attr(e, "intLanes")
                                    .as_deref()
                                    .map(split_ids)
                                    .unwrap_or_default(),
                                shape: attr(e, "shape")
                                    .as_deref()
                                    .map(parse_shape)
                                    .unwrap_or_default(),
                                requests: Vec::new(),
                            };
                            if empty {
                                net.junctions.push(junction);
                            } else {
                                pending = Some(Pending::Junction(junction));
                            }
                        }
                    }
                    "request" => {
                        if let Some(Pending::Junction(junction)) = pending.as_mut() {
                            junction.requests.push(RawRequest {
                                index: attr_u32(e, "index")
                                    .unwrap_or(junction.requests.len() as u32),
                                response: attr(e, "response").unwrap_or_default(),
                                foes: attr(e, "foes").unwrap_or_default(),
                                cont: attr(e, "cont")
                                    .map(|c| c == "1" || c == "true")
                                    .unwrap_or(false),
                            });
                        }
                    }
                    "connection" => {
                        if let (Some(from), Some(to)) = (attr(e, "from"), attr(e, "to")) {
                            net.connections.push(RawConnection {
                                from,
                                to,
                                from_lane: attr_u32(e, "fromLane").unwrap_or(0),
                                to_lane: attr_u32(e, "toLane").unwrap_or(0),
                                via: attr(e, "via"),
                                dir: attr(e, "dir"),
                                state: attr(e, "state"),
                                tl: attr(e, "tl"),
                                link_index: attr_u32(e, "linkIndex"),
                                allow: attr(e, "allow"),
                                disallow: attr(e, "disallow"),
                            });
                        }
                    }
                    "tlLogic" => {
                        if let Some(id) = attr(e, "id") {
                            let logic = RawTlLogic {
                                id,
                                program_id: attr(e, "programID").unwrap_or_default(),
                                logic_type: attr(e, "type").unwrap_or_default(),
                                offset_s: attr_f(e, "offset").unwrap_or(0.0),
                                phases: Vec::new(),
                            };
                            if empty {
                                net.tl_logics.push(logic);
                            } else {
                                pending = Some(Pending::TlLogic(logic));
                            }
                        }
                    }
                    "phase" => {
                        if let Some(Pending::TlLogic(logic)) = pending.as_mut() {
                            let duration = attr_f(e, "duration").unwrap_or(0.0);
                            let state = attr(e, "state").unwrap_or_default();
                            logic.phases.push((duration, state));
                        }
                    }
                    "prohibition" => {
                        if let (Some(prohibitor), Some(prohibited)) =
                            (attr(e, "prohibitor"), attr(e, "prohibited"))
                        {
                            net.prohibitions.push(RawProhibition {
                                prohibitor,
                                prohibited,
                            });
                        }
                    }
                    "roundabout" => {
                        if let Some(nodes) = attr(e, "nodes") {
                            net.roundabouts.push(split_ids(&nodes));
                        }
                    }
                    _ => {}
                }
            }
            quick_xml::events::Event::End(ref e) => {
                if matches!(e.name().into_inner(), "edge" | "junction" | "tlLogic") {
                    match pending.take() {
                        Some(Pending::Edge(edge)) => net.edges.push(edge),
                        Some(Pending::Junction(junction)) => net.junctions.push(junction),
                        Some(Pending::TlLogic(logic)) => net.tl_logics.push(logic),
                        None => {}
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    if !saw_net_root {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: "not a SUMO network: no <net> root element".to_string(),
        });
    }
    // Sorting by id is what makes every later stage's iteration order independent of the
    // file's. A duplicate id keeps the first element.
    net.types.sort_by(|a, b| a.id.cmp(&b.id));
    net.types.dedup_by(|a, b| a.id == b.id);
    net.edges.sort_by(|a, b| a.id.cmp(&b.id));
    net.edges.dedup_by(|a, b| a.id == b.id);
    net.junctions.sort_by(|a, b| a.id.cmp(&b.id));
    net.junctions.dedup_by(|a, b| a.id == b.id);
    net.tl_logics
        .sort_by(|a, b| a.id.cmp(&b.id).then(a.program_id.cmp(&b.program_id)));
    for edge in &mut net.edges {
        edge.lanes.sort_by_key(|l| l.index);
    }
    for junction in &mut net.junctions {
        junction.requests.sort_by_key(|r| r.index);
    }
    Ok(net)
}

/// An attribute's value, unescaped.
fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    for attribute in e.attributes() {
        let Ok(attribute) = attribute else { continue };
        if attribute.key.into_inner() == key {
            return Some(
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| attribute.value.clone().into_owned()),
            );
        }
    }
    None
}

/// An attribute parsed as a finite float.
fn attr_f(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<f64> {
    let v = attr(e, key)?.trim().parse::<f64>().ok()?;
    v.is_finite().then_some(v)
}

/// An attribute parsed as an unsigned integer.
fn attr_u32(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<u32> {
    attr(e, key)?.trim().parse::<u32>().ok()
}

/// An attribute parsed as a signed integer.
fn attr_i64(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<i64> {
    attr(e, key)?.trim().parse::<i64>().ok()
}

/// A `shape` attribute: whitespace-separated `x,y` or `x,y,z` triples.
///
/// A malformed point ends the shape rather than poisoning it: a truncated attribute gives
/// a shorter polyline, which the lane stage then reports, instead of a lane with a
/// coordinate of zero in the middle of it.
pub fn parse_shape(value: &str) -> Vec<Vec3> {
    let mut out = Vec::new();
    for token in value.split_whitespace() {
        let mut parts = token.split(',');
        let (Some(x), Some(y)) = (parts.next(), parts.next()) else {
            break;
        };
        let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>()) else {
            break;
        };
        let z = parts
            .next()
            .and_then(|z| z.parse::<f64>().ok())
            .unwrap_or(0.0);
        if !(x.is_finite() && y.is_finite() && z.is_finite()) {
            break;
        }
        out.push(Vec3::new(x, y, z));
    }
    out
}

/// A `netOffset`-style `x,y` pair.
fn parse_pair(value: &str) -> Option<(f64, f64)> {
    let mut parts = value.split(',');
    let x = parts.next()?.trim().parse::<f64>().ok()?;
    let y = parts.next()?.trim().parse::<f64>().ok()?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// A `convBoundary`-style `min_x,min_y,max_x,max_y`.
fn parse_boundary(value: &str) -> Option<[f64; 4]> {
    let mut out = [0.0f64; 4];
    let mut parts = value.split(',');
    for slot in &mut out {
        *slot = parts.next()?.trim().parse::<f64>().ok()?;
        if !slot.is_finite() {
            return None;
        }
    }
    Some(out)
}

/// A whitespace-separated list of ids.
fn split_ids(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// vClasses, lane kinds, turn codes, link states
// ---------------------------------------------------------------------------

/// The SUMO vClasses this importer maps onto each of the eight
/// [`ClassMask`](crate::model::ClassMask) bits.
///
/// SUMO's list is longer than ours, so several of its classes share one of our bits: the
/// mask is a propagation- and mobility-level statement about who uses a lane, not a
/// permit system. The classes SUMO has and we do not — `ship`, `subway`, `cable_car`,
/// `container`, `aircraft` — map to nothing, which is why a lane that allows only those
/// comes out closed.
const VCLASS_MAP: &[(&str, ClassMask)] = &[
    ("passenger", ClassMask::CAR),
    ("private", ClassMask::CAR),
    ("taxi", ClassMask::CAR),
    ("hov", ClassMask::CAR),
    ("evehicle", ClassMask::CAR),
    ("custom1", ClassMask::CAR),
    ("custom2", ClassMask::CAR),
    ("truck", ClassMask::TRUCK),
    ("trailer", ClassMask::TRUCK),
    ("delivery", ClassMask::TRUCK),
    ("bus", ClassMask::BUS),
    ("coach", ClassMask::BUS),
    ("motorcycle", ClassMask::MOTO),
    ("moped", ClassMask::MOTO),
    ("bicycle", ClassMask::BICYCLE),
    ("pedestrian", ClassMask::PEDESTRIAN),
    ("emergency", ClassMask::EMERGENCY),
    ("authority", ClassMask::EMERGENCY),
    ("army", ClassMask::EMERGENCY),
    ("vip", ClassMask::EMERGENCY),
    ("rail", ClassMask::RAIL),
    ("rail_urban", ClassMask::RAIL),
    ("rail_electric", ClassMask::RAIL),
    ("rail_fast", ClassMask::RAIL),
    ("tram", ClassMask::RAIL),
];

/// The mask a `vClasses` list names.
fn mask_of_vclasses(list: &str) -> ClassMask {
    let mut mask = ClassMask::NONE;
    for token in list.split_whitespace() {
        for (name, bit) in VCLASS_MAP {
            if *name == token {
                mask = mask.union(*bit);
            }
        }
    }
    mask
}

/// The classes a lane or a connection admits, from its `allow` and `disallow`.
///
/// `allow` is authoritative when present; otherwise the mask is everything minus
/// `disallow`; with neither attribute the mask is everything, which is what SUMO's own
/// default means ("all vClasses may use this lane"). Stating both is a contradiction
/// SUMO's own schema does not allow, so `allow` wins and the caller counts it.
fn lane_classes(allow: Option<&str>, disallow: Option<&str>) -> (ClassMask, bool) {
    match (allow, disallow) {
        (Some(a), Some(_)) => (mask_of_vclasses(a), true),
        (Some(a), None) => (mask_of_vclasses(a), false),
        (None, Some(d)) => (ClassMask::ALL.difference(mask_of_vclasses(d)), false),
        (None, None) => (ClassMask::ALL, false),
    }
}

/// What a lane is for, from its edge's function and the classes it admits.
fn lane_kind(function: EdgeFunction, classes: ClassMask) -> LaneKind {
    match function {
        EdgeFunction::Crossing => LaneKind::Crossing,
        EdgeFunction::WalkingArea => LaneKind::Sidewalk,
        EdgeFunction::Internal => LaneKind::Internal,
        _ => {
            let motor = classes.contains_any(ClassMask::MOTOR_TRAFFIC);
            if !motor && classes.contains_any(ClassMask::PEDESTRIAN) {
                LaneKind::Sidewalk
            } else if !motor && classes.contains_any(ClassMask::BICYCLE) {
                LaneKind::Cycle
            } else if classes.contains_any(ClassMask::BUS)
                && !classes.contains_any(ClassMask::CAR)
                && !classes.contains_any(ClassMask::TRUCK)
            {
                LaneKind::Bus
            } else {
                LaneKind::Driving
            }
        }
    }
}

/// The road class a SUMO edge type names.
///
/// netconvert's OSM type ids are `highway.<osm value>` (`osmNetconvert.typ.xml`), so the
/// part after the last dot is the OSM `highway` value and maps onto
/// [`RoadClass`](crate::model::RoadClass) exactly as the OSM importer maps it. Anything
/// else — a Vissim or a hand-written type id — is unclassified rather than guessed.
fn road_class_of_type(edge_type: Option<&str>) -> RoadClass {
    let Some(id) = edge_type else {
        return RoadClass::Unclassified;
    };
    let value = id.rsplit('.').next().unwrap_or(id);
    match value {
        "motorway" => RoadClass::Motorway,
        "trunk" => RoadClass::Trunk,
        "primary" => RoadClass::Primary,
        "secondary" => RoadClass::Secondary,
        "tertiary" => RoadClass::Tertiary,
        "residential" => RoadClass::Residential,
        "living_street" => RoadClass::Living,
        "service" | "unsurfaced" => RoadClass::Service,
        "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" | "tertiary_link" => {
            RoadClass::Link
        }
        "footway" | "pedestrian" | "steps" => RoadClass::Footway,
        "cycleway" => RoadClass::Cycleway,
        "path" | "track" | "bridleway" => RoadClass::Path,
        _ => RoadClass::Unclassified,
    }
}

/// The turn a `<connection dir>` code names.
///
/// The codes are SUMO's own, which is what [`TurnDirection`](crate::model::TurnDirection)
/// was named for: `s` straight, `l` left, `r` right, `L` partially left, `R` partially
/// right, `t` turnaround.
fn turn_of_dir(dir: &str) -> Option<TurnDirection> {
    match dir {
        "s" => Some(TurnDirection::Straight),
        "l" => Some(TurnDirection::Left),
        "r" => Some(TurnDirection::Right),
        "L" => Some(TurnDirection::SlightLeft),
        "R" => Some(TurnDirection::SlightRight),
        "t" => Some(TurnDirection::UTurn),
        _ => None,
    }
}

/// True if a `<connection state>` code is one of the documented ones.
///
/// The states are recorded rather than acted on: a minor link's obligation to give way is
/// in the junction's `<request>` rows, which is where our model keeps it too, so mapping
/// the letter to anything would double-count it. The check exists so that an unfamiliar
/// letter is reported instead of ignored.
fn known_link_state(state: &str) -> bool {
    matches!(
        state,
        "M" | "m" | "$" | "=" | "s" | "w" | "Z" | "o" | "O" | "-" | "G" | "g" | "y" | "u" | "r"
    )
}

/// The signal state a `tlLogic` phase letter names.
///
/// SUMO's `s` — "green right-turn arrow, but stop first" — has no equivalent in
/// [`SignalState`](crate::model::SignalState), which distinguishes only protected from
/// permissive green, so it becomes
/// [`SignalState::GreenYield`](crate::model::SignalState::GreenYield): a movement that may
/// go while giving way, which is the behaviour the junction's conflict matrix then
/// enforces. Everything else is the inverse of
/// [`SignalState::sumo_letter`](crate::model::SignalState::sumo_letter).
fn signal_state_of_letter(c: char) -> Option<crate::model::SignalState> {
    use crate::model::SignalState;
    match c {
        'r' => Some(SignalState::Red),
        'u' => Some(SignalState::RedAmber),
        'y' | 'Y' => Some(SignalState::Amber),
        'G' => Some(SignalState::Green),
        'g' | 's' => Some(SignalState::GreenYield),
        'o' => Some(SignalState::FlashingAmber),
        'O' => Some(SignalState::Off),
        _ => None,
    }
}

/// The junction control a `<junction type>` names, and whether it is an approximation.
fn control_of_junction_type(junction_type: &str) -> (JunctionControl, bool, bool) {
    match junction_type {
        "traffic_light" | "traffic_light_unregulated" | "traffic_light_right_on_red" => {
            // Signalised, but only once a programme has been found for it; the caller
            // decides, so the fallback is priority.
            (JunctionControl::Priority, false, true)
        }
        "priority" => (JunctionControl::Priority, false, false),
        "priority_stop" => (JunctionControl::Stop, true, false),
        "allway_stop" => (JunctionControl::Stop, false, false),
        "right_before_left" | "left_before_right" => (JunctionControl::Priority, true, false),
        "zipper" => (JunctionControl::Priority, true, false),
        "dead_end" => (JunctionControl::Uncontrolled, false, false),
        "unregulated" | "district" | "internal" | "" => {
            (JunctionControl::Uncontrolled, false, false)
        }
        "rail_signal" | "rail_crossing" => (JunctionControl::Priority, true, false),
        _ => (JunctionControl::Priority, true, false),
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// The largest on-grid value not greater than `v`.
///
/// The world's frame corner must be rounded **down**, so that geometry the caller asked
/// for cannot land a millimetre on the wrong side of the origin.
fn floor_to_grid(v: f64, quantum: f64) -> f64 {
    let rounded = quantise(v, quantum);
    if rounded > v {
        quantise(v - quantum, quantum)
    } else {
        rounded
    }
}

/// Drops points closer than one position quantum to their predecessor.
///
/// `Lane::new` refuses a centreline with two points closer than a millimetre
/// (vwp-v1 §4.3), and a `net.xml` shape frequently repeats a point where two geometry
/// segments meet, so the dedupe is the difference between importing a network and
/// rejecting it.
fn dedupe_points(points: Vec<Vec3>) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::with_capacity(points.len());
    for p in points {
        if out
            .last()
            .is_some_and(|q: &Vec3| q.distance(p) < Q_POSITION_M)
        {
            continue;
        }
        out.push(p);
    }
    out
}

/// True if a polyline crosses itself somewhere other than at a shared vertex.
///
/// The world model forbids a self-intersecting lane centreline, and a `net.xml` written
/// by a hand edit or a lane-shape override can hold one.
fn self_intersects(points: &[Vec3]) -> bool {
    if points.len() < 4 {
        return false;
    }
    for i in 0..points.len() - 1 {
        // `j` starts two segments along, so segments that share a vertex are not compared.
        for j in i + 2..points.len() - 1 {
            if crate::index::polylines_cross(&points[i..=i + 1], &points[j..=j + 1]) {
                return true;
            }
        }
    }
    false
}

/// The length of a polyline, metres.
fn polyline_length(points: &[Vec3]) -> f64 {
    points.windows(2).map(|p| p[0].distance(p[1])).sum()
}

/// The junction id whose name is the longest prefix of `id`.
///
/// An internal, crossing or walking-area edge is named `:<junction>_<n>`, and a junction
/// id may itself contain an underscore, so splitting on the separator is ambiguous and
/// the longest match is the only safe rule.
fn junction_by_prefix(id: &str, junctions: &BTreeMap<String, JunctionId>) -> Option<JunctionId> {
    let bare = id.strip_prefix(':').unwrap_or(id);
    let mut best: Option<(usize, JunctionId)> = None;
    for (name, jid) in junctions {
        if bare.starts_with(name.as_str())
            && bare.len() > name.len()
            && bare.as_bytes()[name.len()] == b'_'
            && best.is_none_or(|(len, _)| name.len() > len)
        {
            best = Some((name.len(), *jid));
        }
    }
    best.map(|(_, jid)| jid)
}

/// A SUMO lane id split into its edge id and its lane index.
fn split_lane_id(lane_id: &str) -> Option<(&str, u32)> {
    let (edge, index) = lane_id.rsplit_once('_')?;
    Some((edge, index.parse::<u32>().ok()?))
}

// ---------------------------------------------------------------------------
// The import
// ---------------------------------------------------------------------------

/// One movement, after its two lanes and its connector geometry are known but before the
/// connector lane exists.
///
/// Connector lanes have to be created after every street lane, because the id assignment
/// says so, and a movement's junction is known long before that, so the movement is
/// carried in this form in between.
#[derive(Debug, Clone)]
struct PendingMovement {
    junction: JunctionId,
    from_lane: LaneId,
    to_lane: LaneId,
    from_edge: String,
    to_edge: String,
    /// Empty when the network has no internal lane for this movement.
    geometry: Vec<Vec3>,
    turn: TurnDirection,
    link_index: Option<u32>,
    tl: Option<String>,
    permitted: bool,
    width_m: f64,
    speed_mps: f64,
    classes: ClassMask,
    approach_heading: f64,
}

/// One row of a junction's conflict matrix, with what it takes to match a
/// `<prohibition>` against it.
///
/// A named struct rather than a tuple because it is carried per junction and read twice,
/// and because `(usize, Option<u32>, String, String)` says nothing about which string is
/// which.
#[derive(Debug, Clone)]
struct MatrixRow {
    /// The movement's row and column in the junction's [`ConflictMatrix`].
    row: usize,
    /// Its `linkIndex`, which is the index of its `<request>` row.
    link_index: Option<u32>,
    /// The edge it leaves.
    from_edge: String,
    /// The edge it enters.
    to_edge: String,
}

/// One link a traffic-light programme controls, as the signal stage needs it.
#[derive(Debug, Clone)]
struct ControlledLink {
    /// The lane a plan lists in [`SignalPlan::controlled`]: the movement's connector, or
    /// its approach lane when the network has no connector for it.
    lane: LaneId,
    /// Its `linkIndex`, which is its position in every `<phase state>` string.
    link_index: Option<u32>,
    /// The `tl` attribute: which programme controls it.
    tl: Option<String>,
}

/// Imports a SUMO network file from disk.
///
/// Returns the world and the report of everything the importer had to approximate. The
/// file's SHA-256 becomes the provenance's `source_id`, so a world can always be traced
/// back to the exact bytes it came from.
///
/// ```no_run
/// use v2xw_world::sumo::{SumoOptions, import_sumo_net};
/// let options = SumoOptions::default().imported_at("2026-09-22T00:00:00Z");
/// let (world, report) = import_sumo_net("worlds/cache/grid.net.xml", &options)?;
/// println!("{}", report.to_text());
/// assert!(world.counts().lanes > 0);
/// # Ok::<(), v2xw_world::WorldError>(())
/// ```
///
/// # Errors
///
/// [`WorldError::Io`] if the file cannot be read, [`WorldError::Malformed`] if it is not a
/// well-formed `<net>` document or holds no geometry at all,
/// [`WorldError::InvalidParameter`] for options that cannot be satisfied, and whatever the
/// world model rejects. Bad *data* inside a well-formed network is reported, not raised.
pub fn import_sumo_net(
    path: impl AsRef<Path>,
    options: &SumoOptions,
) -> Result<(crate::model::World, SumoImportReport)> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    import_sumo_net_bytes(&bytes, &path.display().to_string(), options)
}

/// Imports a SUMO network document already in memory.
///
/// `source_id` is what the report and the provenance call it — a path, a URL, or a test's
/// name.
///
/// # Errors
///
/// As [`import_sumo_net`], without the I/O case.
pub fn import_sumo_net_bytes(
    xml: &[u8],
    source_id: &str,
    options: &SumoOptions,
) -> Result<(crate::model::World, SumoImportReport)> {
    options.validate()?;
    let net = parse_sumo_net(xml)?;
    let digest = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(xml);
        hasher.finalize()
    };
    let mut report = SumoImportReport {
        source_id: source_id.to_string(),
        source_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        source_bytes: xml.len() as u64,
        ..SumoImportReport::default()
    };
    let world = import_parsed(&net, options, &mut report)?;
    Ok((world, report))
}

/// Turns a parsed network into a world.
#[allow(clippy::too_many_lines)]
fn import_parsed(
    net: &SumoNet,
    options: &SumoOptions,
    report: &mut SumoImportReport,
) -> Result<crate::model::World> {
    use crate::model::{
        LayerLicence, RoadNetwork, SignalHead, SignalHeadKind, SignalPhase, SymbolTable,
        Transformation, World, WorldProvenance, WorldSourceKind,
    };

    report.net_version = net.version.clone();
    report.location = net.location;
    report.requested_bounds_m = options.bounds_m;
    report.counts.sumo_edges = net.edges.len() as u64;
    report.counts.sumo_junctions = net.junctions.len() as u64;
    report.counts.sumo_connections = net.connections.len() as u64;
    report.counts.sumo_tl_logics = net.tl_logics.len() as u64;
    report.counts.sumo_prohibitions = net.prohibitions.len() as u64;
    report.counts.sumo_roundabouts = net.roundabouts.len() as u64;
    for edge in &net.edges {
        report.counts.sumo_lanes += edge.lanes.len() as u64;
        if edge.function == EdgeFunction::Internal {
            report.counts.sumo_internal_edges += 1;
        }
    }
    for junction in &net.junctions {
        if junction.junction_type == "internal" {
            report.counts.sumo_internal_junctions += 1;
        }
    }

    // A document with no geometry is a failed conversion, not an empty network: importing
    // it would produce a valid, empty, content-addressed world that is indistinguishable
    // from a legitimate one (the OSM importer's R8, the same defect).
    if net.edges.is_empty() || net.junctions.is_empty() {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!(
                "the network holds {} edge(s) and {} junction(s), so there is nothing to \
                 import",
                net.edges.len(),
                net.junctions.len()
            ),
        });
    }

    // --- stage 1: the frame -------------------------------------------------------
    let (frame, frame_corner) = match (options.bounds_m, net.location) {
        (Some(b), _) => (SumoFrameRule::RequestedBounds, Some((b[0], b[1]))),
        (None, Some(l)) => (
            SumoFrameRule::ConvBoundary,
            Some((l.conv_boundary[0], l.conv_boundary[1])),
        ),
        (None, None) => (SumoFrameRule::ImportedGeometry, None),
    };
    let corner = frame_corner.unwrap_or_else(|| {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        for edge in &net.edges {
            for lane in &edge.lanes {
                for p in &lane.shape {
                    min_x = min_x.min(p.x);
                    min_y = min_y.min(p.y);
                }
            }
        }
        for junction in &net.junctions {
            min_x = min_x.min(junction.position.x);
            min_y = min_y.min(junction.position.y);
        }
        if min_x.is_finite() && min_y.is_finite() {
            (min_x, min_y)
        } else {
            (0.0, 0.0)
        }
    });
    let shift = (
        -floor_to_grid(corner.0, Q_POSITION_M),
        -floor_to_grid(corner.1, Q_POSITION_M),
    );
    report.frame = frame;
    report.shift_m = (
        quantise(shift.0, Q_POSITION_M),
        quantise(shift.1, Q_POSITION_M),
    );
    let tx = |p: Vec3| Vec3::new(p.x + shift.0, p.y + shift.1, p.z);
    let in_bounds = |points: &[Vec3]| match options.bounds_m {
        None => true,
        Some([min_x, min_y, max_x, max_y]) => points
            .iter()
            .any(|p| p.x >= min_x && p.x <= max_x && p.y >= min_y && p.y <= max_y),
    };

    // --- stage 2: which edges and junctions are kept ------------------------------
    let mut kept_edges: Vec<&RawEdge> = Vec::new();
    for edge in &net.edges {
        if edge.function != EdgeFunction::Normal && edge.function != EdgeFunction::Connector {
            continue;
        }
        let mut points: Vec<Vec3> = edge.shape.clone();
        for lane in &edge.lanes {
            points.extend(lane.shape.iter().copied());
        }
        if !in_bounds(&points) {
            report.note(SumoAnomaly::OutsideBounds, &edge.id);
            continue;
        }
        kept_edges.push(edge);
    }
    let mut referenced: BTreeSet<&str> = BTreeSet::new();
    for edge in &kept_edges {
        for end in [edge.from.as_deref(), edge.to.as_deref()] {
            if let Some(id) = end {
                referenced.insert(id);
            }
        }
    }
    let mut junction_of: BTreeMap<String, JunctionId> = BTreeMap::new();
    let mut kept_junctions: Vec<&RawJunction> = Vec::new();
    for junction in &net.junctions {
        if junction.junction_type == "internal" {
            continue;
        }
        let keep = referenced.contains(junction.id.as_str())
            || in_bounds(core::slice::from_ref(&junction.position));
        if !keep {
            report.note(SumoAnomaly::OutsideBounds, &junction.id);
            continue;
        }
        kept_junctions.push(junction);
    }
    // Ids in sorted-id order, which `parse_sumo_net` already put the list in.
    for (index, junction) in kept_junctions.iter().enumerate() {
        junction_of.insert(junction.id.clone(), JunctionId::new(index as u32));
    }

    let mut symbols = SymbolTable::new();
    let mut lanes: Vec<Lane> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut junctions: Vec<Junction> = kept_junctions
        .iter()
        .enumerate()
        .map(|(index, raw)| Junction {
            id: JunctionId::new(index as u32),
            position: tx(raw.position),
            shape: Vec::new(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            internal: Vec::new(),
            control: JunctionControl::Uncontrolled,
            conflicts: ConflictMatrix::new(0),
            name: Some(symbols.intern(&raw.id)),
        })
        .collect();

    // --- stage 3: street edges and their lanes ------------------------------------
    // `(edge id, lane index) -> lane id`, so a connection can resolve its two lanes.
    let mut lane_of: BTreeMap<(String, u32), LaneId> = BTreeMap::new();
    for raw in &kept_edges {
        let (Some(from), Some(to)) = (raw.from.as_deref(), raw.to.as_deref()) else {
            report.note(SumoAnomaly::MissingJunction, &raw.id);
            continue;
        };
        let (Some(&from_j), Some(&to_j)) = (junction_of.get(from), junction_of.get(to)) else {
            report.note(SumoAnomaly::MissingJunction, &raw.id);
            continue;
        };
        if raw.lanes.is_empty() {
            report.note(SumoAnomaly::EdgeWithoutLanes, &raw.id);
            continue;
        }
        let edge_type = raw.edge_type.as_deref().and_then(|t| net.edge_type(t));
        // Build every lane's attributes first: an edge whose lanes all fail must not be
        // pushed, and a lane must never reference an edge that was not.
        struct Candidate {
            index: u32,
            kind: LaneKind,
            geometry: Vec<Vec3>,
            width_m: f64,
            speed_mps: f64,
            classes: ClassMask,
            sumo_id: String,
        }
        let mut candidates: Vec<Candidate> = Vec::with_capacity(raw.lanes.len());
        for lane in &raw.lanes {
            let mut geometry = dedupe_points(lane.shape.iter().copied().map(tx).collect());
            if geometry.len() < 2 {
                geometry = dedupe_points(raw.shape.iter().copied().map(tx).collect());
                if geometry.len() < 2 {
                    report.note(SumoAnomaly::LaneWithoutShape, &lane.id);
                    continue;
                }
                report.note(SumoAnomaly::LaneShapeTooShort, &lane.id);
            }
            if self_intersects(&geometry) {
                report.note(SumoAnomaly::SelfIntersectingLane, &lane.id);
                continue;
            }
            let speed_mps = match lane.speed_mps.or(edge_type.and_then(|t| t.speed_mps)) {
                Some(v) if v > 0.0 => {
                    if lane.speed_mps.is_some() {
                        report.counts.speeds_from_lane += 1;
                    } else {
                        report.counts.speeds_from_type += 1;
                    }
                    v
                }
                _ => {
                    report.note(SumoAnomaly::SpeedDefaulted, &lane.id);
                    options.default_speed_mps
                }
            };
            let width_m = match lane.width_m {
                Some(w) if w > 0.0 => {
                    report.counts.widths_from_lane += 1;
                    w
                }
                _ => {
                    report.note(SumoAnomaly::WidthDefaulted, &lane.id);
                    options.default_lane_width_m
                }
            };
            let (classes, both) = lane_classes(
                lane.allow
                    .as_deref()
                    .or(edge_type.and_then(|t| t.allow.as_deref())),
                lane.disallow
                    .as_deref()
                    .or(edge_type.and_then(|t| t.disallow.as_deref())),
            );
            if both {
                report.note(SumoAnomaly::AllowAndDisallow, &lane.id);
            }
            if let Some(stated) = lane.length_m {
                let actual = polyline_length(&geometry);
                if (stated - actual).abs() > 0.05 + 0.01 * actual {
                    report.note(SumoAnomaly::LengthDisagreesWithShape, &lane.id);
                }
            }
            candidates.push(Candidate {
                index: lane.index,
                kind: lane_kind(raw.function, classes),
                geometry,
                width_m,
                speed_mps,
                classes,
                sumo_id: lane.id.clone(),
            });
        }
        if candidates.is_empty() {
            report.note(SumoAnomaly::EdgeWithoutLanes, &raw.id);
            continue;
        }
        let edge_id = EdgeId::new(edges.len() as u32);
        let mut lane_ids = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let lane_id = LaneId::new(lanes.len() as u32);
            match Lane::new(
                lane_id,
                edge_id,
                None,
                u8::try_from(candidate.index).unwrap_or(u8::MAX),
                candidate.kind,
                candidate.geometry,
                candidate.width_m,
                candidate.speed_mps,
                candidate.classes,
            ) {
                Ok(lane) => {
                    lanes.push(lane);
                    lane_ids.push(lane_id);
                    lane_of.insert((raw.id.clone(), candidate.index), lane_id);
                    junctions[to_j.as_usize()].incoming.push(lane_id);
                    junctions[from_j.as_usize()].outgoing.push(lane_id);
                }
                Err(_) => report.note(SumoAnomaly::LaneShapeTooShort, &candidate.sumo_id),
            }
        }
        if lane_ids.is_empty() {
            report.note(SumoAnomaly::EdgeWithoutLanes, &raw.id);
            continue;
        }
        edges.push(Edge {
            id: edge_id,
            from: from_j,
            to: to_j,
            lanes: lane_ids,
            name: Some(symbols.intern(&raw.id)),
            road_class: road_class_of_type(raw.edge_type.as_deref()),
        });
    }

    // --- stage 4: movements, with their connector geometry ------------------------
    // Every connection whose `from` is an internal edge is the second half of a movement
    // the first half already described, so it is followed through `via` rather than
    // imported on its own.
    let mut internal_step: BTreeMap<(String, u32), &RawConnection> = BTreeMap::new();
    for connection in &net.connections {
        let from_internal = net
            .edge(&connection.from)
            .is_some_and(|e| e.function == EdgeFunction::Internal);
        if from_internal {
            internal_step.insert((connection.from.clone(), connection.from_lane), connection);
        }
    }

    let mut movements: Vec<PendingMovement> = Vec::new();
    for connection in &net.connections {
        let Some(from_edge) = net.edge(&connection.from) else {
            report.note(SumoAnomaly::DanglingConnection, &connection.from);
            continue;
        };
        if from_edge.function == EdgeFunction::Internal {
            continue;
        }
        let Some(&from_lane) = lane_of.get(&(connection.from.clone(), connection.from_lane)) else {
            // The edge may have been dropped by the bounds filter, which is already
            // counted; a connection into a kept edge that has no such lane is not.
            if options.bounds_m.is_none() {
                report.note(SumoAnomaly::DanglingConnection, &connection.from);
            }
            continue;
        };
        let Some(&to_lane) = lane_of.get(&(connection.to.clone(), connection.to_lane)) else {
            if options.bounds_m.is_none() {
                report.note(SumoAnomaly::DanglingConnection, &connection.to);
            }
            continue;
        };
        let Some(junction) = from_edge
            .to
            .as_deref()
            .and_then(|j| junction_of.get(j))
            .copied()
        else {
            report.note(SumoAnomaly::OrphanInternalLane, &connection.from);
            continue;
        };

        // The connector: the `via` lane, plus every internal lane the chain continues
        // through. A chain of more than one internal lane means the movement crosses an
        // internal junction, which our model has no place for, so the lanes are
        // concatenated into one connector and the fold is counted.
        let mut geometry: Vec<Vec3> = Vec::new();
        let mut width_m = lanes[from_lane.as_usize()].width_m;
        let mut speed_mps = lanes[to_lane.as_usize()].speed_limit_mps;
        if let Some(via) = connection.via.as_deref() {
            let mut cursor = via.to_string();
            for step in 0..8 {
                let Some((edge_id, lane_index)) = split_lane_id(&cursor) else {
                    report.note(SumoAnomaly::DanglingVia, &cursor);
                    break;
                };
                let Some(internal_edge) = net.edge(edge_id) else {
                    report.note(SumoAnomaly::DanglingVia, &cursor);
                    break;
                };
                let Some(internal_lane) =
                    internal_edge.lanes.iter().find(|l| l.index == lane_index)
                else {
                    report.note(SumoAnomaly::DanglingVia, &cursor);
                    break;
                };
                if step == 0 {
                    if let Some(w) = internal_lane.width_m {
                        width_m = w;
                    }
                    if let Some(v) = internal_lane.speed_mps {
                        speed_mps = v;
                    }
                } else {
                    report.note(SumoAnomaly::InternalJunctionCollapsed, &cursor);
                }
                geometry.extend(internal_lane.shape.iter().copied().map(tx));
                // The next link of the chain, resolved into an owned id before `cursor` is
                // reassigned: `edge_id` borrows from `cursor`.
                let next = internal_step
                    .get(&(edge_id.to_string(), lane_index))
                    .and_then(|c| c.via.clone());
                match next {
                    Some(id) => cursor = id,
                    None => break,
                }
            }
            geometry = dedupe_points(geometry);
            if geometry.len() < 2 || self_intersects(&geometry) {
                geometry.clear();
            }
        }

        let approach = &lanes[from_lane.as_usize()];
        let approach_heading = approach.heading_at(approach.length_m);
        let turn = match connection.dir.as_deref() {
            Some(dir) => match turn_of_dir(dir) {
                Some(turn) => turn,
                None => {
                    report.note(SumoAnomaly::UnknownTurnDirection, &connection.from);
                    TurnDirection::Straight
                }
            },
            None => {
                report.note(SumoAnomaly::TurnDirectionInferred, &connection.from);
                let out = &lanes[to_lane.as_usize()];
                TurnDirection::from_heading_change(crate::model::normalise_angle(
                    out.heading_at(0.0) - approach_heading,
                ))
            }
        };
        if let Some(state) = connection.state.as_deref() {
            if !known_link_state(state) {
                report.note(SumoAnomaly::UnknownLinkState, &connection.from);
            }
        }
        let mut permitted = true;
        if connection.allow.is_some() || connection.disallow.is_some() {
            let (mask, _) =
                lane_classes(connection.allow.as_deref(), connection.disallow.as_deref());
            if mask.is_empty() {
                report.note(SumoAnomaly::ConnectionAllowsNothing, &connection.from);
                permitted = false;
            } else {
                // A `Connection` carries no class mask, so a movement open to some classes
                // and closed to others cannot be expressed: it stays permitted and the
                // loss is counted.
                report.note(SumoAnomaly::ConnectionAccessDropped, &connection.from);
            }
        }
        movements.push(PendingMovement {
            junction,
            from_lane,
            to_lane,
            from_edge: connection.from.clone(),
            to_edge: connection.to.clone(),
            geometry,
            turn,
            link_index: connection.link_index,
            tl: connection.tl.clone(),
            permitted,
            width_m,
            speed_mps,
            classes: lanes[from_lane.as_usize()].allowed,
            approach_heading,
        });
    }
    // Movement order, and therefore connector-lane order and conflict-matrix row order:
    // by link index where the network states one, then by the two lane ids, which is a
    // total order on movements at one junction.
    movements.sort_by_key(|m| {
        (
            m.junction.index(),
            m.link_index.unwrap_or(u32::MAX),
            m.from_lane.index(),
            m.to_lane.index(),
        )
    });
    report.counts.movements = movements.len() as u64;

    // --- stage 5: connector lanes, connections, conflict matrices -----------------
    let mut connections: Vec<Connection> = Vec::new();
    // Per junction: the matrix row of each movement that has a connector, with the
    // movement's link index and its two edge ids, for the prohibition pass.
    let mut rows_by_junction: Vec<Vec<MatrixRow>> = vec![Vec::new(); junctions.len()];
    // Per junction, per movement: the lane a signal plan controls, and the movement's
    // link index and traffic light.
    let mut controlled_by_junction: Vec<Vec<ControlledLink>> = vec![Vec::new(); junctions.len()];

    for j in 0..junctions.len() {
        let internal_edge = EdgeId::new(edges.len() as u32);
        let mut internal_lane_ids: Vec<LaneId> = Vec::new();
        let mut geometry_movements: Vec<crate::procedural::graph::MovementGeometry> = Vec::new();
        let mut rows: Vec<MatrixRow> = Vec::new();

        for movement in movements.iter().filter(|m| m.junction.as_usize() == j) {
            if movement.geometry.is_empty() {
                report.note(
                    SumoAnomaly::MovementWithoutInternalLane,
                    &movement.from_edge,
                );
                connections.push(Connection {
                    from_lane: movement.from_lane,
                    to_lane: movement.to_lane,
                    via: None,
                    direction: movement.turn,
                    permitted: movement.permitted,
                });
                controlled_by_junction[j].push(ControlledLink {
                    lane: movement.from_lane,
                    link_index: movement.link_index,
                    tl: movement.tl.clone(),
                });
                continue;
            }
            let internal = LaneId::new(lanes.len() as u32);
            let lane = match Lane::new(
                internal,
                internal_edge,
                Some(JunctionId::new(j as u32)),
                u8::try_from(internal_lane_ids.len()).unwrap_or(u8::MAX),
                LaneKind::Internal,
                movement.geometry.clone(),
                movement.width_m,
                movement.speed_mps,
                movement.classes,
            ) {
                Ok(lane) => lane,
                Err(_) => {
                    report.note(SumoAnomaly::LaneShapeTooShort, &movement.from_edge);
                    connections.push(Connection {
                        from_lane: movement.from_lane,
                        to_lane: movement.to_lane,
                        via: None,
                        direction: movement.turn,
                        permitted: movement.permitted,
                    });
                    continue;
                }
            };
            lanes.push(lane);
            rows.push(MatrixRow {
                row: internal_lane_ids.len(),
                link_index: movement.link_index,
                from_edge: movement.from_edge.clone(),
                to_edge: movement.to_edge.clone(),
            });
            geometry_movements.push(crate::procedural::graph::MovementGeometry {
                from_lane: movement.from_lane,
                to_lane: movement.to_lane,
                internal,
                turn: movement.turn,
                approach_heading: movement.approach_heading,
                phase_group: 0,
            });
            internal_lane_ids.push(internal);
            controlled_by_junction[j].push(ControlledLink {
                lane: internal,
                link_index: movement.link_index,
                tl: movement.tl.clone(),
            });
            connections.push(Connection {
                from_lane: movement.from_lane,
                to_lane: movement.to_lane,
                via: Some(internal),
                direction: movement.turn,
                permitted: movement.permitted,
            });
            connections.push(Connection {
                from_lane: internal,
                to_lane: movement.to_lane,
                via: None,
                direction: movement.turn,
                permitted: movement.permitted,
            });
        }

        if !internal_lane_ids.is_empty() {
            edges.push(Edge {
                id: internal_edge,
                from: JunctionId::new(j as u32),
                to: JunctionId::new(j as u32),
                lanes: internal_lane_ids.clone(),
                name: None,
                road_class: RoadClass::Internal,
            });
        }

        // The conflict matrix: read from the network's own `<request>` rows when they
        // line up with the movements, computed from the geometry when they do not.
        let raw_junction = kept_junctions[j];
        let matrix = matrix_from_requests(raw_junction, &rows, report);
        junctions[j].conflicts = match matrix {
            Some(matrix) => {
                report.counts.matrices_from_requests += 1;
                matrix
            }
            None => {
                if !rows.is_empty() {
                    report.counts.matrices_from_geometry += 1;
                }
                crate::procedural::graph::conflict_matrix(&geometry_movements, &lanes)
            }
        };
        junctions[j].internal = internal_lane_ids;
        rows_by_junction[j] = rows;
    }

    // --- stage 6: prohibitions ----------------------------------------------------
    for prohibition in &net.prohibitions {
        let Some((pr_from, pr_to)) = prohibition.prohibitor.split_once("->") else {
            report.note(SumoAnomaly::ProhibitionUnmatched, &prohibition.prohibitor);
            continue;
        };
        let Some((pd_from, pd_to)) = prohibition.prohibited.split_once("->") else {
            report.note(SumoAnomaly::ProhibitionUnmatched, &prohibition.prohibited);
            continue;
        };
        let mut applied = false;
        for j in 0..junctions.len() {
            let rows = &rows_by_junction[j];
            let prohibitor: Vec<usize> = rows
                .iter()
                .filter(|r| r.from_edge == pr_from && r.to_edge == pr_to)
                .map(|r| r.row)
                .collect();
            let prohibited: Vec<usize> = rows
                .iter()
                .filter(|r| r.from_edge == pd_from && r.to_edge == pd_to)
                .map(|r| r.row)
                .collect();
            if prohibitor.is_empty() || prohibited.is_empty() {
                continue;
            }
            for a in &prohibited {
                for b in &prohibitor {
                    if a == b {
                        continue;
                    }
                    junctions[j].conflicts.set_foe(*a, *b, true);
                    junctions[j].conflicts.set_response(*a, *b, true);
                    applied = true;
                }
            }
        }
        if applied {
            report.counts.prohibitions_applied += 1;
        } else {
            report.note(SumoAnomaly::ProhibitionUnmatched, &prohibition.prohibited);
        }
    }

    // --- stage 7: signal plans ----------------------------------------------------
    let mut signals: Vec<SignalPlan> = Vec::new();
    let mut seen_programme: BTreeSet<&str> = BTreeSet::new();
    for logic in &net.tl_logics {
        if !seen_programme.insert(logic.id.as_str()) {
            report.note(SumoAnomaly::ExtraSignalProgramme, &logic.id);
            continue;
        }
        let phases: Vec<&(f64, String)> = logic
            .phases
            .iter()
            .filter(|(duration, _)| {
                if *duration > 0.0 {
                    true
                } else {
                    report.note(SumoAnomaly::ZeroDurationPhase, &logic.id);
                    false
                }
            })
            .collect();
        if phases.is_empty() {
            continue;
        }
        // The durations are put on the time grid here and the cycle is their sum, so that
        // `World::validate`'s "phases sum to the cycle" check cannot fail by the rounding
        // the builder's own quantisation would otherwise introduce.
        let durations: Vec<f64> = phases
            .iter()
            .map(|(duration, _)| quantise(*duration, Q_TIME_S))
            .collect();
        let cycle_s: f64 = quantise(durations.iter().sum(), Q_TIME_S);
        if cycle_s <= 0.0 {
            continue;
        }
        let mut touched = 0usize;
        for j in 0..junctions.len() {
            let mut controlled: Vec<(LaneId, u32)> = controlled_by_junction[j]
                .iter()
                .filter(|c| c.link_index.is_some() && c.tl.as_deref() == Some(logic.id.as_str()))
                .map(|c| (c.lane, c.link_index.unwrap_or(0)))
                .collect();
            if controlled.is_empty() {
                continue;
            }
            // Sorted by link index, which is the order the `state` string is in. Two
            // movements may legitimately share an approach lane — each link is its own
            // character — so nothing is deduplicated here.
            controlled.sort_by_key(|(lane, link)| (*link, lane.index()));
            touched += 1;
            let highest = controlled.iter().map(|(_, link)| *link).max().unwrap_or(0);
            if phases
                .iter()
                .any(|(_, state)| state.chars().count() <= highest as usize)
            {
                report.note(SumoAnomaly::SignalStateTooShort, &logic.id);
                continue;
            }
            let mut built_phases: Vec<SignalPhase> = Vec::with_capacity(phases.len());
            for (index, (_, state)) in phases.iter().enumerate() {
                let letters: Vec<char> = state.chars().collect();
                let mut states = Vec::with_capacity(controlled.len());
                for (_, link) in &controlled {
                    let letter = letters.get(*link as usize).copied().unwrap_or('r');
                    states.push(match signal_state_of_letter(letter) {
                        Some(s) => s,
                        None => {
                            report.note(SumoAnomaly::UnknownSignalState, &logic.id);
                            crate::model::SignalState::Red
                        }
                    });
                }
                built_phases.push(SignalPhase {
                    duration_s: durations[index],
                    states,
                    name: None,
                });
            }
            // The head's group is the first phase in which the movement may enter, which
            // is the nearest thing to a phase group a `tlLogic` states.
            let mut heads: Vec<SignalHead> = Vec::new();
            let mut seen_lane: Vec<LaneId> = Vec::new();
            for (position, (lane, _)) in controlled.iter().enumerate() {
                let approach = lanes[lane.as_usize()].end();
                if seen_lane.contains(lane) {
                    continue;
                }
                seen_lane.push(*lane);
                let group = built_phases
                    .iter()
                    .position(|phase| {
                        phase
                            .states
                            .get(position)
                            .is_some_and(|state| state.permits_entry())
                    })
                    .unwrap_or(0);
                heads.push(SignalHead {
                    lane: *lane,
                    position: Vec3::new(
                        approach.x,
                        approach.y,
                        approach.z + options.signal_head_height_m,
                    ),
                    kind: SignalHeadKind::Vehicle,
                    group: u16::try_from(group).unwrap_or(0),
                });
            }
            let plan_id = SignalId::new(signals.len() as u32);
            // `[0, cycle)`, which is what `World::validate` requires: a negative offset
            // wraps forward, and a value that lands exactly on the cycle is zero.
            let offset_s = {
                let wrapped = quantise(logic.offset_s % cycle_s, Q_TIME_S);
                let forward = if wrapped < 0.0 {
                    wrapped + cycle_s
                } else {
                    wrapped
                };
                if forward >= cycle_s { 0.0 } else { forward }
            };
            signals.push(SignalPlan {
                id: plan_id,
                junction: JunctionId::new(j as u32),
                cycle_s,
                offset_s,
                controlled: controlled.iter().map(|(lane, _)| *lane).collect(),
                phases: built_phases,
                heads,
            });
            junctions[j].control = JunctionControl::Signalised { plan: plan_id };
        }
        if touched > 1 {
            report.note(SumoAnomaly::TlsSplitAcrossJunctions, &logic.id);
        }
    }
    report.counts.signal_plans = signals.len() as u64;

    // --- stage 8: junction control, shapes, roundabouts ---------------------------
    for (j, raw) in kept_junctions.iter().enumerate() {
        let (fallback, approximated, wants_signal) = control_of_junction_type(&raw.junction_type);
        if !matches!(junctions[j].control, JunctionControl::Signalised { .. }) {
            junctions[j].control = fallback;
            if wants_signal {
                report.note(SumoAnomaly::SignalisedWithoutProgramme, &raw.id);
            }
        }
        if approximated {
            report.note(SumoAnomaly::JunctionTypeApproximated, &raw.id);
        }
        if !KNOWN_JUNCTION_TYPES.contains(&raw.junction_type.as_str()) {
            report.note(SumoAnomaly::UnknownJunctionType, &raw.id);
        }
        // The junction area: the file's own shape, its position and every lane end that
        // meets it, hulled. Our junction area is convex and must contain the junction's
        // position, which a `net.xml` shape need not be and need not do.
        let mut hull_points: Vec<Vec3> = vec![junctions[j].position];
        hull_points.extend(raw.shape.iter().copied().map(tx));
        for lane in &junctions[j].incoming {
            hull_points.push(lanes[lane.as_usize()].end());
        }
        for lane in &junctions[j].outgoing {
            hull_points.push(lanes[lane.as_usize()].start());
        }
        let hull = crate::model::convex_hull_ring(&hull_points);
        if !raw.shape.is_empty() && hull.len().saturating_sub(1) < raw.shape.len() {
            report.note(SumoAnomaly::JunctionShapeHulled, &raw.id);
        }
        junctions[j].shape = hull;
        junctions[j].incoming.sort_unstable();
        junctions[j].incoming.dedup();
        junctions[j].outgoing.sort_unstable();
        junctions[j].outgoing.dedup();
    }
    for nodes in &net.roundabouts {
        for node in nodes {
            match junction_of.get(node) {
                Some(id) => {
                    if !matches!(
                        junctions[id.as_usize()].control,
                        JunctionControl::Signalised { .. }
                    ) {
                        junctions[id.as_usize()].control = JunctionControl::Roundabout;
                    }
                }
                None => report.note(SumoAnomaly::RoundaboutNodeUnknown, node),
            }
        }
    }

    // --- stage 9: crossings and walking areas -------------------------------------
    let mut crossings: Vec<Crossing> = Vec::new();
    if options.pedestrian_layer {
        for raw in &net.edges {
            match raw.function {
                EdgeFunction::Crossing => {
                    let Some(junction) = junction_by_prefix(&raw.id, &junction_of) else {
                        report.note(SumoAnomaly::CrossingWithoutJunction, &raw.id);
                        continue;
                    };
                    let Some(lane) = raw.lanes.first() else {
                        report.note(SumoAnomaly::EdgeWithoutLanes, &raw.id);
                        continue;
                    };
                    let shape = dedupe_points(lane.shape.iter().copied().map(tx).collect());
                    let (Some(first), Some(last)) = (shape.first(), shape.last()) else {
                        report.note(SumoAnomaly::LaneWithoutShape, &raw.id);
                        continue;
                    };
                    if first.distance(*last) < Q_POSITION_M {
                        report.note(SumoAnomaly::LaneShapeTooShort, &raw.id);
                        continue;
                    }
                    crossings.push(Crossing {
                        id: CrossingId::new(crossings.len() as u32),
                        junction,
                        from: *first,
                        to: *last,
                        width_m: lane.width_m.unwrap_or(options.default_lane_width_m),
                        // netconvert writes no priority on a crossing edge; SUMO's own
                        // default is that a crossing has priority over the turning traffic
                        // that must give way to it, which is what `true` means here.
                        priority: true,
                    });
                }
                EdgeFunction::WalkingArea => {
                    let Some(junction) = junction_by_prefix(&raw.id, &junction_of) else {
                        report.note(SumoAnomaly::WalkingAreaWithoutJunction, &raw.id);
                        continue;
                    };
                    let Some(lane) = raw.lanes.first() else {
                        report.note(SumoAnomaly::EdgeWithoutLanes, &raw.id);
                        continue;
                    };
                    let geometry = dedupe_points(lane.shape.iter().copied().map(tx).collect());
                    if geometry.len() < 2 || self_intersects(&geometry) {
                        report.note(SumoAnomaly::LaneWithoutShape, &raw.id);
                        continue;
                    }
                    let edge_id = EdgeId::new(edges.len() as u32);
                    let lane_id = LaneId::new(lanes.len() as u32);
                    match Lane::new(
                        lane_id,
                        edge_id,
                        Some(junction),
                        0,
                        LaneKind::Sidewalk,
                        geometry,
                        lane.width_m.unwrap_or(options.default_lane_width_m),
                        lane.speed_mps.unwrap_or(WALKING_SPEED_MPS),
                        ClassMask::PEDESTRIAN,
                    ) {
                        Ok(built) => {
                            lanes.push(built);
                            edges.push(Edge {
                                id: edge_id,
                                from: junction,
                                to: junction,
                                lanes: vec![lane_id],
                                name: Some(symbols.intern(&raw.id)),
                                road_class: RoadClass::Footway,
                            });
                        }
                        Err(_) => report.note(SumoAnomaly::LaneShapeTooShort, &raw.id),
                    }
                }
                _ => {}
            }
        }
    }

    // --- counts -------------------------------------------------------------------
    report.counts.junctions = junctions.len() as u64;
    report.counts.edges = edges.len() as u64;
    report.counts.lanes = lanes.len() as u64;
    report.counts.connections = connections.len() as u64;
    report.counts.banned_movements = connections
        .iter()
        .filter(|c| !c.permitted && c.via.is_some())
        .count() as u64;
    report.counts.crossings = crossings.len() as u64;
    report.counts.signalised_junctions = junctions
        .iter()
        .filter(|j| matches!(j.control, JunctionControl::Signalised { .. }))
        .count() as u64;
    for lane in &lanes {
        if lane.admits(ClassMask::MOTOR_TRAFFIC) {
            report.counts.drivable_lanes += 1;
        }
        match lane.kind {
            LaneKind::Internal => report.counts.internal_lanes += 1,
            LaneKind::Sidewalk => report.counts.sidewalk_lanes += 1,
            _ => {}
        }
    }
    for j in 0..junctions.len() {
        let arms: BTreeSet<u32> = edges
            .iter()
            // An edge that starts and ends at the same junction is its connector bundle or
            // one of its walking areas, not an arm.
            .filter(|e| e.from != e.to)
            .filter(|e| e.from.as_usize() == j || e.to.as_usize() == j)
            .map(|e| {
                // A two-way street is two edges between the same pair, so an arm is a
                // junction pair rather than an edge.
                if e.from.as_usize() == j {
                    e.to.index()
                } else {
                    e.from.index()
                }
            })
            .collect();
        if arms.len() >= 3 {
            report.counts.major_junctions += 1;
        }
    }

    // --- provenance ---------------------------------------------------------------
    let geodetic = net.location.and_then(|l| l.geodetic_boundary());
    let origin = match geodetic {
        Some(b) => GeoOrigin::new(b.min_lat_deg, b.min_lon_deg, 0.0),
        None => GeoOrigin::NULL_ISLAND,
    };
    report.geodetic_bbox = geodetic;
    if geodetic.is_some() {
        report.note(SumoAnomaly::ProjectionNotEvaluated, "location");
    } else {
        report.note(SumoAnomaly::NoGeodeticAnchor, "location");
    }

    let mut provenance = WorldProvenance::new(
        WorldSourceKind::SumoNet,
        format!("sha256:{}", report.source_sha256),
        options.import.imported_at.clone(),
        origin,
    );
    provenance.source_bbox = geodetic;
    provenance
        .tool_versions
        .insert(MODEL_ID.to_string(), MODEL_VERSION.to_string());
    if let Some(version) = &net.version {
        provenance
            .tool_versions
            .insert("sumo-net-format".to_string(), version.clone());
    }
    provenance.record(
        Transformation::new("frame")
            .with("rule", frame.label())
            .with("explanation", frame.rule())
            .with("shift_x_m", report.shift_m.0)
            .with("shift_y_m", report.shift_m.1),
    );
    if let Some([min_x, min_y, max_x, max_y]) = options.bounds_m {
        provenance.record(
            Transformation::new("bounds-filter")
                .with("mode", "keep-whole")
                .with("min_x_m", min_x)
                .with("min_y_m", min_y)
                .with("max_x_m", max_x)
                .with("max_y_m", max_y)
                .with(
                    "note",
                    "elements are kept or dropped whole; geometry is not cut at the \
                     boundary",
                ),
        );
    }
    provenance.record(
        Transformation::new("geodetic-anchor")
            .with(
                "rule",
                match geodetic {
                    Some(_) => "south-west corner of <location origBoundary>",
                    None => "null island: the network carries no geodetic information",
                },
            )
            .with("projection", crate::model::Projection::NAME)
            .with(
                "note",
                "the metre coordinates stay in the network's own projection, which is not \
                 this crate's local tangent plane; <location projParameter> is not \
                 evaluated and the disagreement between the two frames is UNVERIFIED",
            ),
    );
    provenance.record(
        Transformation::new("internal-lane-chain")
            .with("rule", "one connector per movement; chains concatenated")
            .with(
                "internal_junctions_dropped",
                report.counts.sumo_internal_junctions,
            )
            .with(
                "collapsed",
                report.anomaly(SumoAnomaly::InternalJunctionCollapsed),
            ),
    );
    provenance.record(
        Transformation::new("junction-shape")
            .with(
                "rule",
                "convex hull of the file's shape, the position and the lane ends",
            )
            .with("hulled", report.anomaly(SumoAnomaly::JunctionShapeHulled)),
    );
    provenance.record(
        Transformation::new("right-of-way")
            .with("from_requests", report.counts.matrices_from_requests)
            .with("from_geometry", report.counts.matrices_from_geometry)
            .with("prohibitions_applied", report.counts.prohibitions_applied)
            .with(
                "note",
                "a banned turn is the absence of a <connection> and needs no record; \
                 <request> and <prohibition> become the conflict matrix",
            ),
    );
    provenance.record(
        Transformation::new("quantise")
            .with("position_m", crate::quant::Q_POSITION_M)
            .with("height_m", crate::quant::Q_HEIGHT_M)
            .with("speed_mps", crate::quant::Q_SPEED_MPS)
            .with("time_s", crate::quant::Q_TIME_S)
            .with("db", crate::quant::Q_DB),
    );
    provenance.record(
        Transformation::new("environment-class")
            .with("rule", "the caller's default for the whole world")
            .with("class", options.import.default_env.label())
            .with(
                "calibration",
                "TODO: calibrate (04-models.md §1.1): the class should be defaulted from \
                 lane density and compared against hand-labelled classes on the Phase 2 \
                 scenarios; it is not, and one class covers the world",
            ),
    );
    // What a SUMO import loses, per the table in 04-models.md §1.1.
    provenance.record_dropped("buildings", 0);
    provenance.record_dropped("landuse_zones", 0);
    provenance.record_dropped("material_classes", 0);
    provenance.record_dropped("internal_junctions", report.counts.sumo_internal_junctions);
    if !options.pedestrian_layer {
        let skipped = net
            .edges
            .iter()
            .filter(|e| {
                e.function == EdgeFunction::WalkingArea || e.function == EdgeFunction::Crossing
            })
            .count() as u64;
        provenance.record_dropped("pedestrian_edges", skipped);
    }
    let mut roads = LayerLicence::new("roads", options.roads_licence.clone());
    roads.attribution = options.roads_attribution.clone();
    provenance.layers.push(roads);
    if options.roads_licence == UNSTATED_LICENCE {
        provenance.notes.push(
            "A SUMO net.xml carries no licence of its own (04-models.md §1.1), and the \
             network's data licence depends on where it came from — ODbL for an OSM \
             conversion, something else for a hand-drawn network. The caller did not state \
             one, so none is claimed."
                .to_string(),
        );
    }
    provenance.notes.push(
        "Buildings, land use and material classes are not in the net.xml format \
         (04-models.md §1.1), so this world has none; terrain is whatever z the lane \
         shapes carried, plus a DEM if one was attached."
            .to_string(),
    );

    let road_network = RoadNetwork::new(lanes, edges, junctions, connections, crossings)?;
    let mut builder = World::builder(origin)
        .roads(road_network)
        .signals(signals)
        .default_env(options.import.default_env)
        .symbols(symbols)
        .provenance(provenance)
        .index_options(options.import.index_options);
    if let Some(terrain) = options.terrain.clone() {
        builder = builder.terrain(terrain);
    }
    let world = builder.build()?;
    if !world.bbox.is_empty() {
        report.extent_m = Some((
            quantise(world.bbox.max.x - world.bbox.min.x, Q_POSITION_M),
            quantise(world.bbox.max.y - world.bbox.min.y, Q_POSITION_M),
        ));
    }
    Ok(world)
}

/// The walking speed a pedestrian lane gets when the network states none, m/s.
///
/// 1.39 m/s is 5 km/h, the walking pace convention of 04-models.md §2.5, which the OSM
/// importer uses for the same purpose.
const WALKING_SPEED_MPS: f64 = 1.39;

/// Every `<junction type>` this importer recognises, so that an unfamiliar one is
/// reported rather than silently approximated.
const KNOWN_JUNCTION_TYPES: &[&str] = &[
    "traffic_light",
    "traffic_light_unregulated",
    "traffic_light_right_on_red",
    "priority",
    "priority_stop",
    "allway_stop",
    "right_before_left",
    "left_before_right",
    "zipper",
    "dead_end",
    "unregulated",
    "district",
    "internal",
    "rail_signal",
    "rail_crossing",
    "",
];

/// The conflict matrix a junction's `<request>` rows describe, or `None` when they do not
/// line up with the movements the importer built.
///
/// `rows` is the junction's [`MatrixRow`] list in matrix-row order. The
/// matrix is read only when **every** row has a link index, the junction has a request
/// for it, and every bitstring is long enough to address every link: a partial read would
/// be a matrix that is wrong in a way nothing downstream could detect.
fn matrix_from_requests(
    raw: &RawJunction,
    rows: &[MatrixRow],
    report: &mut SumoImportReport,
) -> Option<ConflictMatrix> {
    if rows.is_empty() || raw.requests.is_empty() {
        return None;
    }
    let mut request_of: BTreeMap<u32, &RawRequest> = BTreeMap::new();
    for request in &raw.requests {
        request_of.insert(request.index, request);
    }
    let links: Vec<u32> = rows
        .iter()
        .map(|r| r.link_index.unwrap_or(u32::MAX))
        .collect();
    if links.iter().any(|link| *link == u32::MAX) {
        report.note(SumoAnomaly::RequestCountMismatch, &raw.id);
        return None;
    }
    let highest = links.iter().copied().max().unwrap_or(0) as usize;
    for link in &links {
        let Some(request) = request_of.get(link) else {
            report.note(SumoAnomaly::RequestCountMismatch, &raw.id);
            return None;
        };
        if request.response.chars().count() <= highest || request.foes.chars().count() <= highest {
            report.note(SumoAnomaly::RequestStringTooShort, &raw.id);
            return None;
        }
    }
    let mut matrix = ConflictMatrix::new(rows.len());
    for (a, link_a) in links.iter().enumerate() {
        let request_a = request_of[link_a];
        for (b, link_b) in links.iter().enumerate() {
            if a == b {
                continue;
            }
            let foe = request_a.foe_of(*link_b as usize).unwrap_or(false);
            let responds = request_a.responds_to(*link_b as usize).unwrap_or(false);
            if responds && !foe {
                report.note(SumoAnomaly::ResponseWithoutFoe, &raw.id);
            }
            if foe || responds {
                matrix.set_foe(a, b, true);
            }
            if responds {
                matrix.set_response(a, b, true);
            }
        }
    }
    Some(matrix)
}

// ---------------------------------------------------------------------------
// OpenDRIVE, through netconvert
// ---------------------------------------------------------------------------

/// How the OpenDRIVE importer calls `netconvert`.
///
/// 04-models.md §1.1 and §1.2 make `netconvert --opendrive-files` the OpenDRIVE path:
/// SUMO's own converter is the only implementation of the format this project is willing
/// to depend on, and its conversion log is copied into the provenance so that what it
/// dropped is on the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetconvertOptions {
    /// The binary to run. `netconvert` by default, so it is found on `PATH`; a pinned
    /// installation passes an absolute path.
    pub binary: String,
    /// Arguments appended after the input and output options, for the caller's own
    /// netconvert flags.
    pub extra_args: Vec<String>,
    /// Where the intermediate `net.xml` is written. `None` uses the system temporary
    /// directory.
    ///
    /// The file is named for the SHA-256 of the input, **not** for a clock or a counter,
    /// so two runs over the same OpenDRIVE file write the same path and no part of this
    /// crate reads the wall clock (02-architecture.md §6.1).
    pub output_dir: Option<String>,
    /// Keep the intermediate `net.xml` after the import instead of deleting it.
    pub keep_output: bool,
}

impl Default for NetconvertOptions {
    fn default() -> Self {
        Self {
            binary: "netconvert".to_string(),
            extra_args: Vec::new(),
            output_dir: None,
            keep_output: false,
        }
    }
}

/// How much of the `netconvert` log the provenance keeps, bytes.
///
/// A conversion log for a large OpenDRIVE file is tens of thousands of lines of warnings,
/// and the provenance goes into every artefact this crate writes. 8 KiB holds the summary
/// and the first warnings; the report carries the whole log for the run that produced it.
pub const NETCONVERT_LOG_LIMIT: usize = 8 * 1024;

/// Imports an OpenDRIVE file by converting it with `netconvert` first
/// (04-models.md §1.2).
///
/// The intermediate network is imported by [`import_sumo_net`], so an OpenDRIVE world and
/// a SUMO world differ only in their provenance: the source kind, the converter's version
/// and its log.
///
/// # Errors
///
/// [`WorldError::Io`] if the input cannot be read or `netconvert` cannot be run — a
/// missing binary included, with the name that was tried —
/// [`WorldError::Malformed`] if `netconvert` exits non-zero, and whatever
/// [`import_sumo_net`] rejects.
pub fn import_opendrive(
    path: impl AsRef<Path>,
    options: &SumoOptions,
    netconvert: &NetconvertOptions,
) -> Result<(crate::model::World, SumoImportReport)> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let digest = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        hasher.finalize()
    };
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let dir = match &netconvert.output_dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::env::temp_dir(),
    };
    let out = dir.join(format!("v2xw-opendrive-{hex}.net.xml"));

    let output = std::process::Command::new(&netconvert.binary)
        .arg("--opendrive-files")
        .arg(path)
        .arg("--output-file")
        .arg(&out)
        .args(&netconvert.extra_args)
        .output()
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "could not run {:?} (04-models.md §1.2 makes netconvert the OpenDRIVE \
                     path; install SUMO or set NetconvertOptions::binary): {e}",
                    netconvert.binary
                ),
            )
        })?;
    let mut log = String::new();
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!(
                "netconvert refused {}: exit {:?}; log:\n{log}",
                path.display(),
                output.status.code()
            ),
        });
    }

    let net_bytes = std::fs::read(&out)?;
    let (mut world, mut report) =
        import_sumo_net_bytes(&net_bytes, &path.display().to_string(), options)?;
    if !netconvert.keep_output {
        // A failure to remove a temporary file is not a failure to import a world.
        let _ = std::fs::remove_file(&out);
    }
    report.netconvert_log = Some(log.clone());

    // The provenance is not part of the content hash (see `crate::hash`), so recording
    // the conversion after the fact changes the record without changing the world's
    // identity — which is what lets an OpenDRIVE world and its intermediate `net.xml`
    // hash the same.
    let mut truncated = log;
    if truncated.len() > NETCONVERT_LOG_LIMIT {
        truncated.truncate(NETCONVERT_LOG_LIMIT);
        truncated.push_str("\n… truncated");
    }
    world.provenance.source = crate::model::WorldSourceKind::OpenDrive;
    world.provenance.source_id = format!("sha256:{hex}");
    world
        .provenance
        .tool_versions
        .insert(OPENDRIVE_MODEL_ID.to_string(), MODEL_VERSION.to_string());
    world.provenance.record(
        crate::model::Transformation::new("netconvert")
            .with("binary", netconvert.binary.clone())
            .with("input", "--opendrive-files")
            .with("extra_args", netconvert.extra_args.join(" "))
            .with(
                "note",
                "everything netconvert itself drops is in its log, which is recorded below \
                 (04-models.md §1.1)",
            ),
    );
    world.provenance.notes.push(format!(
        "netconvert conversion log (truncated at {NETCONVERT_LOG_LIMIT} bytes):\n{truncated}"
    ));
    Ok((world, report))
}

// ---------------------------------------------------------------------------
// The plug-in seam
// ---------------------------------------------------------------------------

/// The [`WorldSource`](crate::WorldSource) wrapper around [`import_sumo_net`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SumoSource {
    options: SumoOptions,
}

impl SumoSource {
    /// A source with the default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// A source with the options a scenario chose.
    pub fn with_options(options: SumoOptions) -> Self {
        Self { options }
    }

    /// The options this source will use.
    pub fn options(&self) -> &SumoOptions {
        &self.options
    }
}

impl crate::WorldSource for SumoSource {
    fn card(&self) -> v2xw_core::card::ModelCard {
        card()
    }

    fn build(
        &self,
        src: &crate::WorldSourceSpec,
        opts: &crate::ImportOptions,
    ) -> Result<crate::model::World> {
        match src {
            crate::WorldSourceSpec::SumoNet { path } => {
                let options = SumoOptions {
                    import: opts.clone(),
                    ..self.options.clone()
                };
                Ok(import_sumo_net(path, &options)?.0)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

/// The [`WorldSource`](crate::WorldSource) wrapper around [`import_opendrive`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenDriveSource {
    options: SumoOptions,
    netconvert: NetconvertOptions,
}

impl OpenDriveSource {
    /// A source with the default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// A source with the options a scenario chose.
    pub fn with_options(options: SumoOptions, netconvert: NetconvertOptions) -> Self {
        Self {
            options,
            netconvert,
        }
    }
}

impl crate::WorldSource for OpenDriveSource {
    fn card(&self) -> v2xw_core::card::ModelCard {
        opendrive_card()
    }

    fn build(
        &self,
        src: &crate::WorldSourceSpec,
        opts: &crate::ImportOptions,
    ) -> Result<crate::model::World> {
        match src {
            crate::WorldSourceSpec::OpenDrive { path } => {
                let options = SumoOptions {
                    import: opts.clone(),
                    ..self.options.clone()
                };
                Ok(import_opendrive(path, &options, &self.netconvert)?.0)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Model cards
// ---------------------------------------------------------------------------

/// The model card of `world/source/sumo-net` (03-interfaces.md §12).
pub fn card() -> v2xw_core::card::ModelCard {
    use v2xw_core::card::{
        Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
        ValidationStatus,
    };
    let docs = || {
        Source::new(
            SourceKind::Code,
            "SUMO Road Networks documentation, via 04-models.md §1.2 (R10 §A12): the \
             elements <edge>, <lane>, <junction>, <connection>, <tlLogic>, <request>",
        )
    };
    let decision = || {
        Source::new(
            SourceKind::Code,
            "04-models.md §1.1: SUMO net.xml is the interchange format, and the table of \
             what a SUMO import preserves and loses",
        )
    };
    let todo = |name: &str, unit: &str, default: serde_json::Value, plan: &str| Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: None,
        source: Source::todo_calibrate(format!("sumo-net {name}")),
        calibration: Some(plan.to_string()),
    };
    ModelCard {
        tier: vec![Tier::Abstract, Tier::Medium, Tier::High],
        equations: vec![
            Equation {
                name: "request bit order".to_string(),
                latex_or_text: "bit(s, link) = s[len(s) − 1 − link]".to_string(),
                notes: Some(
                    "SUMO writes a <request>'s response and foes strings with the highest \
                     link index first, so link 0 is the last character. Reading them left \
                     to right imports a junction's right of way exactly backwards."
                        .to_string(),
                ),
            },
            Equation::new(
                "internal chain",
                "one connector lane per movement: the `via` lane's shape, concatenated \
                 with every internal lane the chain continues through",
            ),
            Equation::new(
                "junction area",
                "convex hull of the file's own shape, the junction's position and every \
                 lane end that meets it",
            ),
        ],
        parameters: vec![
            Parameter::new("pedestrian_layer", "-", true.into(), decision()),
            Parameter::new(
                "roads_licence",
                "-",
                UNSTATED_LICENCE.into(),
                Source {
                    kind: SourceKind::Code,
                    reference: "04-models.md §1.1: \"the format has no separate license\""
                        .to_string(),
                    accessed: None,
                    note: Some(
                        "The network's data licence depends on where it came from, so the \
                         importer claims none unless the caller states one."
                            .to_string(),
                    ),
                },
            ),
            todo(
                "default_lane_width_m",
                "m",
                3.2.into(),
                "netconvert omits a lane's `width` when it equals its own default, so this \
                 value stands in for that default on most networks. 3.2 m is SUMO's \
                 lane-width constant as recalled rather than as read from a cached \
                 document: confirm `netconvert --default.lanewidth` for the pinned SUMO \
                 version, record the version, and check the count of \
                 `width-defaulted` lanes in the import report afterwards",
            ),
            todo(
                "default_speed_mps",
                "m/s",
                13.89.into(),
                "a fallback speed limit is a statement about a jurisdiction, exactly as it \
                 is for the OSM importer (V4/W1). A net.xml almost always states every \
                 lane's speed, so this should fire on no lane at all; if the report counts \
                 any `speed-defaulted` lane, the network is unusual and the number matters. \
                 Resolve it the same way the OSM presets were resolved",
            ),
            todo(
                "signal_head_height_m",
                "m",
                5.0.into(),
                "net.xml states no mounting height. Measure mast-arm heights from three \
                 street-level imagery samples per Phase 2 city and record the median, as \
                 the grid generator's card also plans",
            ),
        ],
        assumptions: vec![
            "A `net.xml`'s coordinates are Cartesian metres in the network's own frame, so \
             the importer translates and never projects."
                .to_string(),
            "Lane index 0 is the rightmost in the direction of travel, which is SUMO's own \
             convention and this project's (vwp-v1 §4.3)."
                .to_string(),
            "A banned turn is the absence of a <connection>: netconvert has already applied \
             every turn restriction by the time it writes the network."
                .to_string(),
            "A <request>'s `response` names the links this one gives way to, and every \
             response is also a foe. A response without a foe is reported."
                .to_string(),
            "`allow` beats `disallow` when a lane states both, and a lane that states \
             neither admits every class, which is SUMO's own default."
                .to_string(),
        ],
        limitations: vec![
            "Buildings, land use and material classes are not in the format \
             (04-models.md §1.1), so a SUMO world has none; the propagation environment \
             class is the caller's default for the whole world rather than defaulted from \
             lane density, which §1.1 records as TODO: calibrate."
                .to_string(),
            "An internal junction — the waiting position inside a large intersection — has \
             no equivalent in this model, so a movement's chain of internal lanes is \
             concatenated into one connector and the count is reported. A vehicle \
             therefore cannot wait inside the junction."
                .to_string(),
            "A `<tlLogic>` that controls links at several junctions becomes one plan per \
             junction with the same cycle and phases. The programme's identity is lost, so \
             a coordinated corridor's junctions are no longer visibly one controller."
                .to_string(),
            "A per-connection class restriction cannot be expressed: a Connection carries \
             no class mask, so the movement stays permitted unless it admits no class at \
             all, and the loss is counted."
                .to_string(),
            "Geometry is not cut at a requested boundary — elements are kept or dropped \
             whole — so a bounded import's extent can exceed the request by one edge."
                .to_string(),
            "`<location projParameter>` is not evaluated, so the metre frame stays in \
             SUMO's projection while the geodetic anchor comes from `origBoundary`. The \
             disagreement between that projection and this crate's local tangent plane is \
             UNVERIFIED."
                .to_string(),
            "A walking area is an area in SUMO and a polyline here, so its geometry is the \
             area's outline traversed as a path."
                .to_string(),
        ],
        ignores: vec![
            "`<roundabout edges>`: only its `nodes` are read, to set the junction control."
                .to_string(),
            "Lane `acceleration`, `changeLeft`, `changeRight`, `endOffset`, `customShape` \
             and the `<neigh>` element."
                .to_string(),
            "Actuated and delay-based traffic-light programmes are imported as their \
             static phase list: the plan's `type` is not carried over."
                .to_string(),
        ],
        sources: vec![docs(), decision()],
        validation: Validation::new(ValidationStatus::UnitTested),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            MODEL_ID,
            Family::World,
            MODEL_VERSION,
            "Reads a SUMO net.xml into the same lane-level world the OSM importer \
             produces: edges, lanes, junctions, connections, internal connectors, \
             right-of-way matrices, traffic-light programmes and crossings, with a counted \
             anomaly wherever the two formats disagree.",
        )
    }
}

/// The model card of `world/source/opendrive` (03-interfaces.md §12).
pub fn opendrive_card() -> v2xw_core::card::ModelCard {
    use v2xw_core::card::{Family, Source, SourceKind, Tier};
    let mut card = card();
    card.id = OPENDRIVE_MODEL_ID.to_string();
    card.family = Family::World;
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.purpose = "Reads an ASAM OpenDRIVE file by converting it with `netconvert \
                    --opendrive-files` and importing the result, with the converter's own \
                    log copied into the provenance."
        .to_string();
    card.sources.push(Source::new(
        SourceKind::Code,
        "netconvert import formats, via 04-models.md §1.2 (R10 §A12): plain XML, OSM, \
         VISUM, Vissim, OpenDRIVE, MATSim, SUMO, Shapefile, RoboCup, DlrNavteq/GDF",
    ));
    card.assumptions.push(
        "SUMO is installed: the conversion is `netconvert`'s, not this crate's, and the \
         world is only as good as that converter."
            .to_string(),
    );
    card.limitations.push(
        "Everything netconvert itself drops is dropped here, including the pedestrian \
         network where OpenDRIVE does not carry it as lanes and a signal's timing where \
         the file only references it (04-models.md §1.1). Its log is the record of what \
         was lost."
            .to_string(),
    );
    card.limitations.push(
        "The intermediate network is written to a file named for the input's SHA-256 and \
         deleted afterwards unless the caller keeps it, so the import is reproducible but \
         not hermetic."
            .to_string(),
    );
    card
}

/// Every model card this module publishes.
pub fn model_cards() -> Vec<v2xw_core::card::ModelCard> {
    vec![card(), opendrive_card()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest network that exercises every stage: two streets meeting at a
    /// signalised crossroads, with internal lanes, requests and a programme.
    ///
    /// The geometry is deliberately round: junction `c` is at the origin of the network's
    /// own frame, the four outer nodes are 100 m away on the axes, and every lane is one
    /// per direction, 3.2 m wide, at 13.89 m/s.
    fn crossroads() -> String {
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<net version=\"1.20\">\n\
             <location netOffset=\"0.00,0.00\" convBoundary=\"-100.00,-100.00,100.00,100.00\" \
             origBoundary=\"-0.001,-0.001,0.001,0.001\" projParameter=\"+proj=utm\"/>\n\
             <type id=\"highway.residential\" priority=\"3\" numLanes=\"1\" speed=\"13.89\"/>\n",
        );
        // West arm, both directions.
        s.push_str(
            "<edge id=\"w2c\" from=\"w\" to=\"c\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"w2c_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"-100.00,-1.60 -10.00,-1.60\"/>\n</edge>\n",
        );
        s.push_str(
            "<edge id=\"c2w\" from=\"c\" to=\"w\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"c2w_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"-10.00,1.60 -100.00,1.60\"/>\n</edge>\n",
        );
        // East arm.
        s.push_str(
            "<edge id=\"e2c\" from=\"e\" to=\"c\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"e2c_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"100.00,1.60 10.00,1.60\"/>\n</edge>\n",
        );
        s.push_str(
            "<edge id=\"c2e\" from=\"c\" to=\"e\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"c2e_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"10.00,-1.60 100.00,-1.60\"/>\n</edge>\n",
        );
        // South arm.
        s.push_str(
            "<edge id=\"s2c\" from=\"s\" to=\"c\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"s2c_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"1.60,-100.00 1.60,-10.00\"/>\n</edge>\n",
        );
        s.push_str(
            "<edge id=\"c2s\" from=\"c\" to=\"s\" priority=\"3\" type=\"highway.residential\">\n\
             <lane id=\"c2s_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"-1.60,-10.00 -1.60,-100.00\"/>\n</edge>\n",
        );
        // Two internal connectors at `c`: west-to-east straight on, south-to-east left.
        s.push_str(
            "<edge id=\":c_0\" function=\"internal\">\n\
             <lane id=\":c_0_0\" index=\"0\" speed=\"13.89\" length=\"20.00\" \
             shape=\"-10.00,-1.60 10.00,-1.60\"/>\n</edge>\n",
        );
        s.push_str(
            "<edge id=\":c_1\" function=\"internal\">\n\
             <lane id=\":c_1_0\" index=\"0\" speed=\"9.00\" length=\"14.00\" \
             shape=\"1.60,-10.00 1.60,-1.60 10.00,-1.60\"/>\n</edge>\n",
        );
        // The junctions. `c`'s request rows are written SUMO's way: link 0 is the last
        // character of each bitstring.
        s.push_str(
            "<junction id=\"c\" type=\"traffic_light\" x=\"0.00\" y=\"0.00\" \
             incLanes=\"w2c_0 s2c_0\" intLanes=\":c_0_0 :c_1_0\" \
             shape=\"-10.00,-10.00 10.00,-10.00 10.00,10.00 -10.00,10.00\">\n\
             <request index=\"0\" response=\"00\" foes=\"10\" cont=\"0\"/>\n\
             <request index=\"1\" response=\"01\" foes=\"01\" cont=\"0\"/>\n\
             </junction>\n",
        );
        for (id, x, y) in [("w", -100.0, 0.0), ("e", 100.0, 0.0), ("s", 0.0, -100.0)] {
            s.push_str(&format!(
                "<junction id=\"{id}\" type=\"priority\" x=\"{x:.2}\" y=\"{y:.2}\" \
                 incLanes=\"\" intLanes=\"\" shape=\"\"/>\n"
            ));
        }
        // The two movements, each in its two halves, plus the programme.
        s.push_str(
            "<connection from=\"w2c\" to=\"c2e\" fromLane=\"0\" toLane=\"0\" via=\":c_0_0\" \
             tl=\"c\" linkIndex=\"0\" dir=\"s\" state=\"G\"/>\n\
             <connection from=\":c_0\" to=\"c2e\" fromLane=\"0\" toLane=\"0\" dir=\"s\" \
             state=\"M\"/>\n\
             <connection from=\"s2c\" to=\"c2e\" fromLane=\"0\" toLane=\"0\" via=\":c_1_0\" \
             tl=\"c\" linkIndex=\"1\" dir=\"l\" state=\"g\"/>\n\
             <connection from=\":c_1\" to=\"c2e\" fromLane=\"0\" toLane=\"0\" dir=\"l\" \
             state=\"M\"/>\n",
        );
        s.push_str(
            "<tlLogic id=\"c\" type=\"static\" programID=\"0\" offset=\"0\">\n\
             <phase duration=\"31\" state=\"Gg\"/>\n\
             <phase duration=\"4\" state=\"yy\"/>\n\
             <phase duration=\"25\" state=\"rr\"/>\n</tlLogic>\n",
        );
        s.push_str("</net>\n");
        s
    }

    fn options() -> SumoOptions {
        SumoOptions::default().imported_at("2026-09-22T00:00:00Z")
    }

    fn import(xml: &str) -> (crate::model::World, SumoImportReport) {
        import_sumo_net_bytes(xml.as_bytes(), "fixture", &options()).expect("the fixture imports")
    }

    #[test]
    fn request_bitstrings_are_read_right_to_left() {
        // "10" means link 1 is a foe of this one and link 0 is not.
        assert_eq!(RawRequest::bit("10", 0), Some(false));
        assert_eq!(RawRequest::bit("10", 1), Some(true));
        assert_eq!(RawRequest::bit("10", 2), None);
        let request = RawRequest {
            index: 1,
            response: "01".to_string(),
            foes: "01".to_string(),
            cont: false,
        };
        assert_eq!(request.responds_to(0), Some(true));
        assert_eq!(request.responds_to(1), Some(false));
    }

    #[test]
    fn shapes_and_boundaries_parse() {
        let shape = parse_shape("1.0,2.0 3.0,4.0,5.0");
        assert_eq!(shape.len(), 2);
        assert_eq!(shape[0], Vec3::new(1.0, 2.0, 0.0));
        assert_eq!(shape[1], Vec3::new(3.0, 4.0, 5.0));
        // A truncated point ends the shape rather than poisoning it.
        assert_eq!(parse_shape("1.0,2.0 3.0").len(), 1);
        assert_eq!(parse_shape("").len(), 0);
        assert_eq!(
            parse_boundary("-1.0,-2.0,3.0,4.0"),
            Some([-1.0, -2.0, 3.0, 4.0])
        );
        assert_eq!(parse_boundary("1,2,3"), None);
        assert_eq!(parse_pair("1.5,-2.5"), Some((1.5, -2.5)));
        assert_eq!(split_ids(" a  b\tc "), vec!["a", "b", "c"]);
        assert_eq!(split_lane_id(":c_0_0"), Some((":c_0", 0)));
        assert_eq!(split_lane_id("w2c_0"), Some(("w2c", 0)));
        assert_eq!(split_lane_id("nounderscore"), None);
    }

    #[test]
    fn vclasses_map_onto_the_eight_bits() {
        assert_eq!(mask_of_vclasses("passenger"), ClassMask::CAR);
        assert!(
            mask_of_vclasses("pedestrian").contains_all(ClassMask::PEDESTRIAN),
            "a footway admits pedestrians"
        );
        // A lane that allows only classes we do not model comes out closed.
        assert!(mask_of_vclasses("ship container").is_empty());
        let (mask, both) = lane_classes(None, Some("pedestrian bicycle"));
        assert!(!mask.contains_any(ClassMask::PEDESTRIAN));
        assert!(mask.contains_all(ClassMask::MOTOR_TRAFFIC));
        assert!(!both);
        let (mask, both) = lane_classes(Some("pedestrian"), Some("all"));
        assert_eq!(mask, ClassMask::PEDESTRIAN);
        assert!(both, "stating both is reported");
        assert_eq!(lane_classes(None, None).0, ClassMask::ALL);
    }

    #[test]
    fn lane_kinds_follow_the_classes_and_the_function() {
        assert_eq!(
            lane_kind(EdgeFunction::Normal, ClassMask::MOTOR_TRAFFIC),
            LaneKind::Driving
        );
        assert_eq!(
            lane_kind(EdgeFunction::Normal, ClassMask::PEDESTRIAN),
            LaneKind::Sidewalk
        );
        assert_eq!(
            lane_kind(EdgeFunction::Normal, ClassMask::BICYCLE),
            LaneKind::Cycle
        );
        assert_eq!(
            lane_kind(EdgeFunction::Normal, ClassMask::BUS),
            LaneKind::Bus
        );
        assert_eq!(
            lane_kind(EdgeFunction::Crossing, ClassMask::PEDESTRIAN),
            LaneKind::Crossing
        );
        assert_eq!(
            lane_kind(EdgeFunction::WalkingArea, ClassMask::PEDESTRIAN),
            LaneKind::Sidewalk
        );
    }

    #[test]
    fn a_document_that_is_not_a_network_is_refused() {
        let error = import_sumo_net_bytes(b"<html><body>502</body></html>", "gateway", &options())
            .expect_err("an HTML error page is not a network");
        assert!(matches!(error, WorldError::Malformed { .. }));
        let error = import_sumo_net_bytes(
            b"<?xml version=\"1.0\"?><net version=\"1.20\"/>",
            "empty",
            &options(),
        )
        .expect_err("an empty network is a failed conversion");
        assert!(matches!(error, WorldError::Malformed { .. }));
    }

    #[test]
    fn the_crossroads_imports_with_its_lanes_junctions_and_connectors() {
        let (world, report) = import(&crossroads());
        assert_eq!(report.counts.sumo_junctions, 4);
        assert_eq!(report.counts.sumo_internal_edges, 2);
        assert_eq!(world.counts().junctions, 4);
        // Six street lanes plus two connectors.
        assert_eq!(world.counts().lanes, 8);
        assert_eq!(report.counts.internal_lanes, 2);
        assert_eq!(report.counts.movements, 2);
        // Each movement is two connection records.
        assert_eq!(world.counts().connections, 4);
        // The frame came from convBoundary, whose corner is (-100, -100), so the west
        // node lands on x = 0 and every coordinate is non-negative.
        assert_eq!(report.frame, SumoFrameRule::ConvBoundary);
        assert_eq!(report.shift_m, (100.0, 100.0));
        assert!(world.bbox.min.x >= 0.0 && world.bbox.min.y >= 0.0);
        // No width or speed was defaulted: the fixture states all of them.
        assert_eq!(report.anomaly(SumoAnomaly::WidthDefaulted), 0);
        assert_eq!(report.anomaly(SumoAnomaly::SpeedDefaulted), 0);
        assert_eq!(report.counts.speeds_from_lane, 6);
        assert_eq!(report.counts.widths_from_lane, 6);
    }

    #[test]
    fn the_junctions_right_of_way_comes_from_its_request_rows() {
        let (world, report) = import(&crossroads());
        assert_eq!(report.counts.matrices_from_requests, 1);
        assert_eq!(report.counts.matrices_from_geometry, 0);
        let c = world
            .roads
            .junctions()
            .iter()
            .find(|j| j.internal.len() == 2)
            .expect("the crossroads has two connectors");
        // Row 0 is link 0 (the straight-on), row 1 is link 1 (the left turn).
        assert!(c.conflicts.is_foe(0, 1), "the two movements cross");
        assert!(
            c.conflicts.must_yield(1, 0),
            "the left turn gives way to the straight-on, as its response bit says"
        );
        assert!(
            !c.conflicts.must_yield(0, 1),
            "and the straight-on does not give way to it"
        );
    }

    #[test]
    fn the_programme_becomes_a_plan_whose_phases_sum_to_its_cycle() {
        let (world, report) = import(&crossroads());
        assert_eq!(report.counts.signal_plans, 1);
        let plan = &world.signals[0];
        assert_eq!(plan.phases.len(), 3);
        assert_eq!(plan.controlled.len(), 2);
        assert!((plan.cycle_s - 60.0).abs() < 1e-9, "31 + 4 + 25 = 60");
        assert!((plan.total_phase_duration_s() - plan.cycle_s).abs() <= 1e-9);
        assert_eq!(plan.offset_s, 0.0);
        // "Gg": the straight-on is a protected green, the left turn a permissive one.
        assert_eq!(plan.phases[0].states[0], crate::model::SignalState::Green);
        assert_eq!(
            plan.phases[0].states[1],
            crate::model::SignalState::GreenYield
        );
        assert_eq!(plan.phases[2].states[0], crate::model::SignalState::Red);
        assert!(matches!(
            world.roads.junction(plan.junction).control,
            JunctionControl::Signalised { .. }
        ));
    }

    #[test]
    fn the_import_is_a_pure_function_of_the_bytes() {
        let xml = crossroads();
        let (a, _) = import(&xml);
        let (b, _) = import(&xml);
        assert_eq!(a.content_hash, b.content_hash);
        // The import date is provenance, not geometry, so it cannot move the hash.
        let (dated, _) = import_sumo_net_bytes(
            xml.as_bytes(),
            "fixture",
            &SumoOptions::default().imported_at("1999-01-01"),
        )
        .expect("imports");
        assert_eq!(dated.content_hash, a.content_hash);
    }

    #[test]
    fn requested_bounds_fix_the_frame_and_drop_what_is_outside() {
        let xml = crossroads();
        // A box west of the crossroads: the west arm has a point inside it, the east and
        // south arms have none, so two of the four arms are dropped whole.
        let options = options().bounds_m([-200.0, -200.0, -50.0, 200.0]);
        let (world, report) =
            import_sumo_net_bytes(xml.as_bytes(), "fixture", &options).expect("imports");
        assert_eq!(report.frame, SumoFrameRule::RequestedBounds);
        assert_eq!(report.shift_m, (200.0, 200.0));
        assert!(
            report.anomaly(SumoAnomaly::OutsideBounds) >= 4,
            "the east and south arms are outside the box: {}",
            report.to_text()
        );
        assert!(world.counts().junctions <= 4);
        // The frame is the request's corner, so the surviving geometry is placed relative
        // to it and not to whatever happened to be kept.
        let (again, _) =
            import_sumo_net_bytes(xml.as_bytes(), "fixture", &options).expect("imports");
        assert_eq!(world.content_hash, again.content_hash);
    }

    #[test]
    fn a_lane_without_a_shape_falls_back_to_its_edge_and_is_counted() {
        let xml = crossroads().replace(
            "<lane id=\"w2c_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\" \
             shape=\"-100.00,-1.60 -10.00,-1.60\"/>",
            "<lane id=\"w2c_0\" index=\"0\" speed=\"13.89\" length=\"90.00\" width=\"3.20\"/>",
        );
        let (world, report) = import(&xml);
        assert!(
            report.anomaly(SumoAnomaly::LaneWithoutShape) > 0
                || report.anomaly(SumoAnomaly::LaneShapeTooShort) > 0,
            "the missing shape is reported: {}",
            report.to_text()
        );
        assert!(world.counts().lanes < 8, "the lane could not be built");
    }

    #[test]
    fn a_missing_dir_is_inferred_and_counted() {
        let xml = crossroads().replace("dir=\"s\" state=\"G\"", "state=\"G\"");
        let (_, report) = import(&xml);
        assert_eq!(report.anomaly(SumoAnomaly::TurnDirectionInferred), 1);
    }

    #[test]
    fn a_prohibition_becomes_a_response_bit() {
        let xml = crossroads().replace(
            "</net>",
            "<prohibition prohibitor=\"w2c->c2e\" prohibited=\"s2c->c2e\"/>\n</net>",
        );
        let (world, report) = import(&xml);
        assert_eq!(report.counts.prohibitions_applied, 1);
        let c = world
            .roads
            .junctions()
            .iter()
            .find(|j| j.internal.len() == 2)
            .expect("the crossroads");
        assert!(c.conflicts.must_yield(1, 0));
    }

    #[test]
    fn the_report_names_what_was_lost() {
        let (world, report) = import(&crossroads());
        let text = report.to_text();
        assert!(text.contains("sumo "), "{text}");
        assert!(text.contains("right of way"), "{text}");
        // Buildings, land use and material classes are not in the format at all.
        assert!(world.buildings.is_empty());
        assert!(world.landuse.is_empty());
        assert!(
            world.provenance.dropped.contains_key("buildings"),
            "the provenance records what the format cannot carry"
        );
        assert!(
            world
                .provenance
                .transformations
                .iter()
                .any(|t| t.name == "right-of-way"),
            "and how right of way was resolved"
        );
    }

    #[test]
    fn the_cards_validate_and_name_their_calibration_plans() {
        for card in model_cards() {
            card.validate().expect("the card validates");
            for parameter in card.todo_calibrate() {
                assert!(
                    parameter
                        .calibration
                        .as_deref()
                        .is_some_and(|plan| plan.len() > 40),
                    "{} needs a real calibration plan",
                    parameter.name
                );
            }
        }
    }
}
