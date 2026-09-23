//! `world/source/osm` — the native OpenStreetMap importer (04-models.md §1.2).
//!
//! Reads an OSM XML extract and produces a lane-level [`World`]: a drivable graph with
//! per-lane centrelines, junction areas, internal connectors, conflict matrices and turn
//! restrictions; a pedestrian and cycle graph in the same lane list that no motor vehicle
//! may enter; buildings with sourced heights; land-use zones; synthesised fixed-time
//! signal plans; and a provenance record naming every transformation applied.
//!
//! **Nothing in this module is specific to any city** (D7). Manhattan is the Phase 1
//! world, but it appears only in the tests and in the scenario file; the code reads tags,
//! not place names.
//!
//! # The pipeline
//!
//! | Stage | What it does | Where |
//! |---|---|---|
//! | 1. parse | one streaming pass over the XML, no DOM | [`parse_osm`] |
//! | 2. classify | `highway`, `access`, `oneway`, `lanes`, `maxspeed`, `turn:lanes` → a [`WayPlan`] per way | [`classify_way`] |
//! | 3. project | geodetic → local ENU metres, origin at the south-west corner of the kept geometry (D6) | `Projection` |
//! | 4. split | ways cut at nodes shared by two or more ways of the same family; interior shape nodes stay as geometry | `split_ways` |
//! | 5. collapse | chains of segments through a trivial two-road node merged back into one segment | `collapse_trivial_junctions` |
//! | 6. lay out | lanes offset left to right from the way centreline, trimmed back at each junction | `build_network` |
//! | 7. connect | junction areas, internal connectors, turn directions, conflict matrices, `restriction` relations | `build_movements`, `apply_restrictions` |
//! | 8. signalise | `highway=traffic_signals` nodes → fixed-time plans with the 04-models.md §2.3 defaults | `synthesise_signals` |
//! | 9. buildings, land use, crossings | rings, holes, heights with their [`HeightSource`] | `build_buildings`, `build_landuse` |
//! | 10. record | provenance, licence, attribution, content hash | [`import_osm`] |
//!
//! # What it does not do
//!
//! The osm2streets simplification list (04-models.md §1.2) has four entries. This
//! importer implements **the trivial two-road junction collapse** (stage 5), which is the
//! one that matters most on real data because OSM splits a single street into many ways
//! at every attribute change. The other three — dual-carriageway "sausage link" merging,
//! dog-leg junction merging and parallel footway snapping — are **not** implemented; they
//! are counted as deliberate omissions in [`ImportReport::skipped_simplifications`] and
//! recorded in the provenance, because a simplification that silently did not run would
//! be indistinguishable from one that ran and found nothing.
//!
//! Terrain is flat at `z = 0` in Phase 1. [`OsmOptions::terrain`] is the hook a DEM
//! importer fills (04-models.md §1.4); when it is `None` the provenance says so.
//!
//! # Robustness
//!
//! Real extracts contain ways with one node, ways referencing nodes the extract does not
//! carry, `maxspeed=signals`, `height=12;15`, unclosed multipolygon rings and footprints
//! that self-intersect. None of these may panic or abort the import: every one is counted
//! in an [`ImportReport`] by [`Anomaly`] category, with a few example OSM ids, and the
//! import continues. The report is returned beside the world.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family as CardFamily, ModelCard, Parameter, Source, SourceKind, Tier,
    Validation, ValidationStatus,
};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{BuildingId, EdgeId, JunctionId, LaneId, SignalId};
use v2xw_core::math;

use crate::error::{Result, WorldError};
use crate::model::{
    Building, ClassMask, ConflictMatrix, Connection, Crossing, CrossingId, Edge, GeoBbox,
    GeoOrigin, HeightSource, Junction, JunctionControl, LanduseClass, LanduseZone, Lane, LaneKind,
    LayerLicence, MaterialClass, Projection, RoadClass, RoadNetwork, SignalHead, SignalHeadKind,
    SignalPhase, SignalPlan, SignalState, SymbolId, SymbolTable, Terrain, Transformation,
    TurnDirection, World, WorldProvenance, WorldSourceKind, ZoneId, normalise_angle, simplify_rdp,
};
use crate::quant::{Q_ANGLE_RAD, Q_DEGREES, Q_POSITION_M, Q_TIME_S, quantise};
use crate::{ImportOptions, WorldSource, WorldSourceSpec};

/// The model id of this importer (04-models.md §1.2).
pub const MODEL_ID: &str = "world/source/osm";

/// The importer's own version, as the model card and the provenance report it.
pub const MODEL_VERSION: &str = "1.0.0";

/// The licence every OSM-derived layer carries.
pub const OSM_LICENCE: &str = "ODbL-1.0";

/// The attribution string an OSM-derived layer requires (04-models.md §1.5,
/// 08-measurement-and-data.md §9).
pub const OSM_ATTRIBUTION: &str = "© OpenStreetMap contributors";

// ---------------------------------------------------------------------------
// Anomalies and the import report
// ---------------------------------------------------------------------------

/// One category of thing the importer found wrong with its input, or had to guess.
///
/// Every variant is counted in [`ImportReport::anomalies`] rather than raised as an
/// error: an import of a real city always trips several of these, and an importer that
/// stopped at the first one would never finish. The enum order is the report's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Anomaly {
    /// A way referenced a node the extract does not contain (normal at the cut edge of a
    /// bounding-box extract, and a data error inside it).
    MissingNode,
    /// A way had fewer than two distinct nodes after missing references were dropped.
    WayTooShort,
    /// Two successive way nodes projected to the same millimetre, so one was dropped.
    DuplicateNode,
    /// A `maxspeed` value no rule could parse; the class default was used.
    UnparsableMaxspeed,
    /// `maxspeed=none` (an unlimited motorway); the class default was used.
    MaxspeedNone,
    /// A `lanes*` value that is not a non-negative integer; the class default was used.
    UnparsableLanes,
    /// An `oneway` value outside the documented vocabulary; `no` was assumed.
    UnparsableOneway,
    /// `oneway=reversible` or `alternating`: a tidal-flow street, imported as one-way in
    /// the mapped direction because the world model has no time-varying direction.
    ReversibleOneway,
    /// An odd total `lanes` count on a two-way street with no `lanes:forward`; the extra
    /// lane was given to the forward direction.
    OddLaneSplit,
    /// A `turn:lanes` value with an unknown token, or with a different number of lanes
    /// from the direction it describes; it was ignored and the turns inferred instead.
    UnparsableTurnLanes,
    /// A way whose access tags exclude every class this importer models, so it was
    /// dropped.
    AccessDenied,
    /// A `highway=*` value this importer does not model; the way was dropped.
    UnknownHighwayValue,
    /// A `height`, `min_height` or `width` value no rule could parse.
    UnparsableHeight,
    /// A multi-valued measurement such as `height=12;15`; the first value was taken.
    MultiValuedHeight,
    /// A `building:levels` value that is not a non-negative number.
    UnparsableLevels,
    /// A parsed height outside `(0, 1000]` m; the default rule was used instead.
    ImplausibleHeight,
    /// A building or land-use ring with fewer than three distinct points.
    RingTooShort,
    /// A multipolygon member ring that does not close; the fragment was dropped.
    UnclosedRing,
    /// A multipolygon with no usable outer ring.
    NoOuterRing,
    /// A `restriction` relation missing a `from`, `via` or `to` member.
    RestrictionIncomplete,
    /// A `restriction` relation whose members do not meet at a junction of this world.
    RestrictionUnmatched,
    /// A `restriction=*` value outside the documented vocabulary.
    UnknownRestriction,
    /// A `traffic_signals` node with no junction within [`ImportOptions::guess_signals_m`].
    OrphanTrafficSignal,
    /// A crossing way with no junction within [`OsmOptions::crossing_snap_m`].
    CrossingWithoutJunction,
    /// A segment too short to trim back from both of its junction areas; it was kept at
    /// full length and its lanes overlap the junction polygons.
    SegmentTooShortToTrim,
    /// A lane whose geometry degenerated (under two distinct points after offsetting and
    /// trimming); the lane was dropped.
    DegenerateLane,
    /// An edge that lost every one of its lanes, so the edge itself was dropped.
    EmptyEdgeDropped,
    /// A segment whose two ends are the same junction; it was dropped, because a lane
    /// that leaves and re-enters one junction has no movement through it.
    SelfLoopSegment,
    /// A relation of a `type` this importer does not read.
    UnsupportedRelation,
    /// A way or building dropped because it lies entirely outside the requested bounding
    /// box.
    ClippedOut,
    /// A `maxspeed` tag on a way family for which a speed limit is not meaningful — a
    /// footway, a path, a staircase or a cycleway. The family's own walking or riding
    /// pace was kept instead (R2).
    SpeedTagIgnored,
    /// A lane whose centreline crossed itself after offsetting, and was repaired by
    /// pruning the inverted vertices or the loop they formed (R1).
    ///
    /// Offsetting a bend whose radius is smaller than the lane's own offset inverts the
    /// inner lane; the repair keeps arc length injective in space, which every model
    /// that computes a gap or a leader from `(lane, s)` depends on.
    SelfIntersectingLane,
    /// A driving lane shorter than [`OsmOptions::min_useful_lane_m`] — a junction-trimming
    /// sliver a car-following or lane-change model has no room to act in (V8).
    ShortDrivingLane,
    /// A movement whose approach and departure lanes touch within a millimetre, so no
    /// connector lane could be built for it.
    ///
    /// The movement is still subject to turn restrictions, but it has no row in the
    /// junction's conflict matrix, because the matrix is indexed by connector lane (R3).
    ZeroLengthConnector,
    /// A polygon excluded from the obstacle set because its tags place it below ground or
    /// make it transit infrastructure rather than a building (V2).
    SubsurfaceStructure,
    /// A `building:part` polygon with no parent `building` outline anywhere over it; it
    /// was kept as a building in its own right (V1).
    BuildingPartWithoutOutline,
    /// A `building=*` way that a `building:part` multipolygon borrowed as one of its
    /// rings, kept as a structure of its own instead of being consumed by the part.
    ///
    /// The Empire State Building is mapped this way: relation 10872054 is
    /// `type=multipolygon` + `building:part=yes` for the five-storey base, and its
    /// `outer` member is way 34633854 — the `building=office` outline that carries the
    /// name and the 443.2 m height. Consuming the way deleted the Empire State Building
    /// from the world (V1).
    OutlineInPartRelation,
    /// A building whose `roof:height` is most of its `height`, so the tagged height is a
    /// spire or a mast tip; the obstacle height was cut back to the structural top (V10).
    SpireHeightCapped,
    /// A way whose geometry was cut at the bounding-box boundary (V5).
    ClippedGeometry,
    /// A large share of the drivable lanes took a class default speed far above the
    /// speed limits the source itself states, which is what a preset from the wrong
    /// jurisdiction looks like (V4/W1).
    ///
    /// Fired once per import, with the figures on [`ImportReport::speed_audit`].
    ClassDefaultsAboveTaggedSpeeds,
    /// A `restriction` relation whose `via` node **is** a junction of this world, but
    /// which named no movement through it.
    ///
    /// Kept apart from [`Anomaly::RestrictionUnmatched`] because the two mean different
    /// things: that one says the junction is not in the world, this one says the
    /// junction is there and the `from`/`to`/turn triple did not describe any movement
    /// through it (R3).
    RestrictionNoMovement,
}

impl Anomaly {
    /// Every anomaly category, in report order.
    pub const ALL: [Anomaly; 41] = [
        Anomaly::MissingNode,
        Anomaly::WayTooShort,
        Anomaly::DuplicateNode,
        Anomaly::UnparsableMaxspeed,
        Anomaly::MaxspeedNone,
        Anomaly::UnparsableLanes,
        Anomaly::UnparsableOneway,
        Anomaly::ReversibleOneway,
        Anomaly::OddLaneSplit,
        Anomaly::UnparsableTurnLanes,
        Anomaly::AccessDenied,
        Anomaly::UnknownHighwayValue,
        Anomaly::UnparsableHeight,
        Anomaly::MultiValuedHeight,
        Anomaly::UnparsableLevels,
        Anomaly::ImplausibleHeight,
        Anomaly::RingTooShort,
        Anomaly::UnclosedRing,
        Anomaly::NoOuterRing,
        Anomaly::RestrictionIncomplete,
        Anomaly::RestrictionUnmatched,
        Anomaly::UnknownRestriction,
        Anomaly::OrphanTrafficSignal,
        Anomaly::CrossingWithoutJunction,
        Anomaly::SegmentTooShortToTrim,
        Anomaly::DegenerateLane,
        Anomaly::EmptyEdgeDropped,
        Anomaly::SelfLoopSegment,
        Anomaly::UnsupportedRelation,
        Anomaly::ClippedOut,
        Anomaly::SpeedTagIgnored,
        Anomaly::SelfIntersectingLane,
        Anomaly::ShortDrivingLane,
        Anomaly::ZeroLengthConnector,
        Anomaly::SubsurfaceStructure,
        Anomaly::BuildingPartWithoutOutline,
        Anomaly::OutlineInPartRelation,
        Anomaly::SpireHeightCapped,
        Anomaly::ClippedGeometry,
        Anomaly::ClassDefaultsAboveTaggedSpeeds,
        Anomaly::RestrictionNoMovement,
    ];

    /// A stable kebab-case label, used by the report and the provenance record.
    pub const fn label(self) -> &'static str {
        match self {
            Anomaly::MissingNode => "missing-node",
            Anomaly::WayTooShort => "way-too-short",
            Anomaly::DuplicateNode => "duplicate-node",
            Anomaly::UnparsableMaxspeed => "unparsable-maxspeed",
            Anomaly::MaxspeedNone => "maxspeed-none",
            Anomaly::UnparsableLanes => "unparsable-lanes",
            Anomaly::UnparsableOneway => "unparsable-oneway",
            Anomaly::ReversibleOneway => "reversible-oneway",
            Anomaly::OddLaneSplit => "odd-lane-split",
            Anomaly::UnparsableTurnLanes => "unparsable-turn-lanes",
            Anomaly::AccessDenied => "access-denied",
            Anomaly::UnknownHighwayValue => "unknown-highway-value",
            Anomaly::UnparsableHeight => "unparsable-height",
            Anomaly::MultiValuedHeight => "multi-valued-height",
            Anomaly::UnparsableLevels => "unparsable-levels",
            Anomaly::ImplausibleHeight => "implausible-height",
            Anomaly::RingTooShort => "ring-too-short",
            Anomaly::UnclosedRing => "unclosed-ring",
            Anomaly::NoOuterRing => "no-outer-ring",
            Anomaly::RestrictionIncomplete => "restriction-incomplete",
            Anomaly::RestrictionUnmatched => "restriction-unmatched",
            Anomaly::UnknownRestriction => "unknown-restriction",
            Anomaly::OrphanTrafficSignal => "orphan-traffic-signal",
            Anomaly::CrossingWithoutJunction => "crossing-without-junction",
            Anomaly::SegmentTooShortToTrim => "segment-too-short-to-trim",
            Anomaly::DegenerateLane => "degenerate-lane",
            Anomaly::EmptyEdgeDropped => "empty-edge-dropped",
            Anomaly::SelfLoopSegment => "self-loop-segment",
            Anomaly::UnsupportedRelation => "unsupported-relation",
            Anomaly::ClippedOut => "clipped-out",
            Anomaly::SpeedTagIgnored => "speed-tag-ignored",
            Anomaly::SelfIntersectingLane => "self-intersecting-lane",
            Anomaly::ShortDrivingLane => "short-driving-lane",
            Anomaly::ZeroLengthConnector => "zero-length-connector",
            Anomaly::SubsurfaceStructure => "subsurface-structure",
            Anomaly::BuildingPartWithoutOutline => "building-part-without-outline",
            Anomaly::OutlineInPartRelation => "outline-in-part-relation",
            Anomaly::SpireHeightCapped => "spire-height-capped",
            Anomaly::ClippedGeometry => "clipped-geometry",
            Anomaly::ClassDefaultsAboveTaggedSpeeds => "class-defaults-above-tagged-speeds",
            Anomaly::RestrictionNoMovement => "restriction-no-movement",
        }
    }
}

impl core::fmt::Display for Anomaly {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// How many example OSM ids the report keeps per anomaly category.
pub const ANOMALY_SAMPLES: usize = 4;

/// What the source file contained and what the importer made of it.
///
/// Returned beside the [`World`] by [`import_osm`]. Everything here is derived from the
/// import and is deterministic: two imports of the same bytes produce equal reports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    /// What was imported: the file path, or the caller's label for a byte slice.
    pub source_id: String,
    /// SHA-256 of the source bytes, lower-case hex — the provenance's `source_id`.
    pub source_sha256: String,
    /// Size of the source, bytes.
    pub source_bytes: u64,
    /// The geodetic box the world covers, measured from the world it produced.
    pub bbox: Option<GeoBbox>,
    /// The box the caller asked for, when it asked for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_bbox: Option<GeoBbox>,
    /// The world's own extent, `(east, north)` metres — what to compare against the
    /// requested box (V3/V5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent_m: Option<(f64, f64)>,
    /// What fixed the world frame (V3).
    #[serde(default)]
    pub frame: FrameRule,
    /// How the requested box was applied to way geometry (V3/V5).
    #[serde(default)]
    pub bbox_clip: BboxClip,
    /// Which named `highway=*` default preset supplied the class fallbacks (V4/W1).
    ///
    /// Always `Some` on a report that came back from [`import_osm`]: an import without a
    /// preset does not get this far.
    #[serde(default)]
    pub highway_preset: Option<HighwayPreset>,
    /// Whether the class defaults the preset supplied are consistent with the speed
    /// limits the source itself states (V4/W1).
    #[serde(default)]
    pub speed_audit: SpeedAudit,
    /// The threshold [`ImportCounts::short_driving_lanes`] was counted against, metres
    /// (V8).
    #[serde(default)]
    pub min_useful_lane_m: f64,
    /// What the file held and what came out.
    pub counts: ImportCounts,
    /// Every anomaly category that fired, with its count.
    pub anomalies: BTreeMap<Anomaly, u64>,
    /// Up to [`ANOMALY_SAMPLES`] example OSM element ids per anomaly, for a human.
    pub samples: BTreeMap<Anomaly, Vec<i64>>,
    /// The osm2streets simplifications this importer does not implement, by name.
    pub skipped_simplifications: Vec<String>,
}

/// Whether the class-default speeds the [`HighwayPreset`] supplied look like the
/// jurisdiction the source is in (V4/W1).
///
/// The preset is a choice, and a wrong choice is silent: the lanes that took a class
/// default are exactly the lanes with no tag to contradict it. What the source *does*
/// state is the check. If a large share of the drivable lanes take a class default well
/// above the limits the extract itself carries, the preset belongs to another country —
/// which is what `sumo-german` over Manhattan looks like: 404 of 2 421 driving lanes
/// above 90 km/h in an extract whose own `maxspeed` tags are 25 mph on 456 ways.
///
/// It is a signal, not a verdict: a genuine motorway box would trip it too, which is why
/// [`SpeedAudit::fired`] raises [`Anomaly::ClassDefaultsAboveTaggedSpeeds`] and says so
/// in the report rather than failing the import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeedAudit {
    /// Drivable lanes whose limit came from the way's own `maxspeed`.
    pub tagged_lanes: u64,
    /// Drivable lanes whose limit came from the preset's class default.
    pub defaulted_lanes: u64,
    /// The 95th percentile of the tagged limits, m/s — what the source says the fast end
    /// of this network is. Zero when nothing is tagged.
    pub tagged_p95_mps: f64,
    /// [`SpeedAudit::tagged_p95_mps`] × [`SPEED_AUDIT_FACTOR`], m/s: above this a class
    /// default is faster than anything the source states.
    pub threshold_mps: f64,
    /// Drivable lanes that took a class default above [`SpeedAudit::threshold_mps`].
    pub far_above_lanes: u64,
    /// [`SpeedAudit::far_above_lanes`] over every drivable lane, tagged or not.
    pub far_above_share: f64,
    /// True when the share reached [`SPEED_AUDIT_SHARE`] over a tagged sample of at
    /// least [`SPEED_AUDIT_MIN_TAGGED`] lanes.
    pub fired: bool,
}

/// How far above the source's own stated limits a class default must be before the
/// [`SpeedAudit`] counts it against the preset.
///
/// 1.25 is this importer's choice and is on the model card: a default a quarter faster
/// than the 95th percentile of what the extract itself states is outside the
/// distribution rather than at the top of it. Measured on the Phase 1 extract,
/// `sumo-german`'s trunk/primary/secondary default is 27.78 m/s against a tagged p95 of
/// 11.18 m/s (25 mph) — a factor of 2.49, and 467 of 2 421 drivable lanes above the
/// threshold. `urban-us-nyc` on the same extract leaves 11 lanes above it, the FDR Drive
/// motorway lanes at the 55 mph statutory maximum, which is 0.5 % and does not fire.
pub const SPEED_AUDIT_FACTOR: f64 = 1.25;

/// The share of drivable lanes that must be that far above before the audit fires.
pub const SPEED_AUDIT_SHARE: f64 = 0.05;

/// The smallest tagged sample the audit will draw a conclusion from: below this the
/// percentile is noise and the audit stays quiet.
pub const SPEED_AUDIT_MIN_TAGGED: usize = 20;

/// The tallies of one import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportCounts {
    /// `<node>` elements in the file.
    pub osm_nodes: u64,
    /// `<way>` elements in the file.
    pub osm_ways: u64,
    /// `<relation>` elements in the file.
    pub osm_relations: u64,
    /// Ways classified as drivable.
    pub drivable_ways: u64,
    /// Ways classified as footway, path, steps or pedestrian street.
    pub pedestrian_ways: u64,
    /// Ways classified as cycleway.
    pub cycle_ways: u64,
    /// Segments after splitting at shared nodes, before the trivial-junction collapse.
    pub segments_before_collapse: u64,
    /// Segments after the collapse.
    pub segments_after_collapse: u64,
    /// Junctions removed by the collapse.
    pub junctions_collapsed: u64,
    /// Junctions in the world, of every kind — including the footway intersections of
    /// the sidewalk mesh, which are 77.6 % of them on the Phase 1 extract (V9).
    pub junctions: u64,
    /// Junctions with at least one drivable arm: the ones a vehicular model cares about
    /// (V9). Use [`road_junction_ids`] to iterate them.
    pub road_junctions: u64,
    /// Road junctions with three or more drivable arms — real intersections, as opposed
    /// to a boundary stub or a mid-street attribute change (V9).
    pub major_road_junctions: u64,
    /// Junctions carrying a signal plan.
    pub signalised_junctions: u64,
    /// Signalised junctions counted against [`ImportCounts::major_road_junctions`]
    /// rather than against every junction in the world, which is the figure that means
    /// something (V9).
    pub signalised_major_junctions: u64,
    /// Edges in the world, including the synthetic internal edge of each junction.
    pub edges: u64,
    /// Lanes in the world, of every kind.
    pub lanes: u64,
    /// Lanes a motor vehicle may drive on, including junction connectors.
    pub drivable_lanes: u64,
    /// Lanes of kind [`LaneKind::Sidewalk`].
    pub sidewalk_lanes: u64,
    /// Lanes of kind [`LaneKind::Cycle`].
    pub cycle_lanes: u64,
    /// Junction connector lanes.
    pub internal_lanes: u64,
    /// Drivable lanes whose speed limit came from the way's own `maxspeed` tag.
    pub speeds_tagged: u64,
    /// Drivable lanes whose speed limit came from the [`HighwayPreset`] class default
    /// (V4/W1) — the ones a calibration pass must target, and the ones a reader of this
    /// report must not trust as legal limits.
    pub speeds_defaulted: u64,
    /// Drivable lanes whose width came from a `width:lanes` or `width` tag (V6/W2).
    pub widths_tagged: u64,
    /// Drivable lanes whose width came from the class default or the global option.
    pub widths_defaulted: u64,
    /// Lanes whose centreline crossed itself after offsetting and had to be repaired
    /// (R1).
    pub lanes_repaired: u64,
    /// Driving lanes shorter than [`OsmOptions::min_useful_lane_m`] (V8).
    pub short_driving_lanes: u64,
    /// Connections in the world (each movement appears twice: see [`Connection`]).
    pub connections: u64,
    /// Connections a turn restriction marked not permitted.
    pub banned_connections: u64,
    /// Turn restriction relations applied.
    pub restrictions_applied: u64,
    /// Buildings imported: one per real structure (V1).
    pub buildings: u64,
    /// `building:part` volumes folded into the `building=*` outline that contains them,
    /// and therefore **not** counted again as obstacles (V1).
    ///
    /// "Folded" means the part's **height** is folded in: the outline's obstacle height
    /// is the tallest part's top when that is taller than the outline's own tags say
    /// (V1). Before the fold existed, the part was simply dropped and 4 323 tagged
    /// heights went with it, which left the Chrysler Building 10 m tall.
    pub building_parts_merged: u64,
    /// `building:part` volumes with no parent outline anywhere over them, kept as
    /// buildings in their own right (V1).
    pub building_parts_orphan: u64,
    /// Polygons excluded from the obstacle set because their tags place them below
    /// ground or make them transit infrastructure (V2/W4).
    pub buildings_subsurface: u64,
    /// Buildings whose height was cut back from a spire or mast tip to the structural
    /// top (V10).
    pub buildings_spire_capped: u64,
    /// Interior holes kept across every building.
    pub building_holes: u64,
    /// Buildings whose height came from an explicit `height` tag.
    pub heights_tagged: u64,
    /// Buildings whose height came from `building:levels`.
    pub heights_from_levels: u64,
    /// Outlines whose height came from the tallest `building:part` inside them, because
    /// the outline's own tags described less than the parts did (V1).
    pub heights_from_parts: u64,
    /// Buildings whose height came from the land-use default.
    pub heights_defaulted: u64,
    /// Pedestrian crossings imported.
    pub crossings: u64,
    /// Land-use zones imported.
    pub landuse_zones: u64,
    /// `highway=traffic_signals` nodes found in the file.
    pub traffic_signal_nodes: u64,
    /// Ways whose geometry was cut at the bounding-box boundary (V5).
    pub ways_clipped: u64,
    /// Runs of geometry that survived the clip, across every cut way — more than one
    /// when a way left the box and came back.
    pub clipped_runs: u64,
    /// Building and land-use rings cut at the bounding-box boundary (V5).
    pub polygons_clipped: u64,
}

impl ImportReport {
    /// How many times `kind` fired.
    pub fn anomaly(&self, kind: Anomaly) -> u64 {
        self.anomalies.get(&kind).copied().unwrap_or(0)
    }

    /// Every anomaly, of every category, added up.
    pub fn total_anomalies(&self) -> u64 {
        self.anomalies.values().sum()
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
        if let Some(b) = self.bbox {
            let _ = writeln!(
                s,
                "bbox              {:.6},{:.6} .. {:.6},{:.6}",
                b.min_lon_deg, b.min_lat_deg, b.max_lon_deg, b.max_lat_deg
            );
        }
        if let Some(b) = self.requested_bbox {
            let _ = writeln!(
                s,
                "requested         {:.6},{:.6} .. {:.6},{:.6}",
                b.min_lon_deg, b.min_lat_deg, b.max_lon_deg, b.max_lat_deg
            );
        }
        if self.requested_bbox.is_some() {
            let _ = writeln!(
                s,
                "frame             origin from the {}; bbox {}, {:.0} m margin",
                self.frame.label(),
                self.bbox_clip.label(),
                self.bbox_clip.margin_m()
            );
        } else {
            let _ = writeln!(
                s,
                "frame             origin from the {}; no bbox requested",
                self.frame.label()
            );
        }
        if let Some((east, north)) = self.extent_m {
            let _ = writeln!(
                s,
                "extent            {east:.0} x {north:.0} m = {:.2} km^2",
                east * north / 1e6
            );
        }
        let _ = writeln!(
            s,
            "osm               {} nodes, {} ways, {} relations",
            c.osm_nodes, c.osm_ways, c.osm_relations
        );
        let _ = writeln!(
            s,
            "classified        {} drivable, {} pedestrian, {} cycle ways",
            c.drivable_ways, c.pedestrian_ways, c.cycle_ways
        );
        let _ = writeln!(
            s,
            "segments          {} split -> {} after collapsing {} trivial junctions",
            c.segments_before_collapse, c.segments_after_collapse, c.junctions_collapsed
        );
        let _ = writeln!(
            s,
            "network           {} junctions ({} signalised), {} edges, {} lanes",
            c.junctions, c.signalised_junctions, c.edges, c.lanes
        );
        // V9: the all-junctions figure is dominated by footway intersections, so the
        // signalisation share is quoted against the junctions that have real arms too.
        let _ = writeln!(
            s,
            "road junctions    {} with a drivable arm, {} with 3+ ({} of those signalised)",
            c.road_junctions, c.major_road_junctions, c.signalised_major_junctions
        );
        let _ = writeln!(
            s,
            "lanes             {} drivable, {} internal, {} sidewalk, {} cycle",
            c.drivable_lanes, c.internal_lanes, c.sidewalk_lanes, c.cycle_lanes
        );
        let _ = writeln!(
            s,
            "speed limits      preset {}: {} drivable lanes tagged, {} took the class \
             default",
            self.highway_preset.map_or("(none selected)", HighwayPreset::label),
            c.speeds_tagged,
            c.speeds_defaulted
        );
        // V4/W1: the loud line. A preset from the wrong jurisdiction is invisible in the
        // counts above — every lane it touched is a lane with no tag to contradict it —
        // so the report states what the source itself says and how far the defaults sit
        // outside it, whether or not the audit fired.
        let a = &self.speed_audit;
        if a.fired {
            let _ = writeln!(
                s,
                "speed audit       WARNING {} of {} drivable lanes ({:.1} %) took a class \
                 default above {:.2} m/s, which is {:.2}x the tagged p95 of {:.2} m/s -- \
                 the {} preset does not match this source's jurisdiction",
                a.far_above_lanes,
                a.tagged_lanes + a.defaulted_lanes,
                a.far_above_share * 100.0,
                a.threshold_mps,
                SPEED_AUDIT_FACTOR,
                a.tagged_p95_mps,
                self.highway_preset.map_or("(none selected)", HighwayPreset::label),
            );
        } else {
            let _ = writeln!(
                s,
                "speed audit       ok: {} of {} drivable lanes ({:.1} %) above {:.2} m/s \
                 ({:.2}x the tagged p95 of {:.2} m/s)",
                a.far_above_lanes,
                a.tagged_lanes + a.defaulted_lanes,
                a.far_above_share * 100.0,
                a.threshold_mps,
                SPEED_AUDIT_FACTOR,
                a.tagged_p95_mps,
            );
        }
        let _ = writeln!(
            s,
            "lane widths       {} drivable lanes from a tag, {} from the class default",
            c.widths_tagged, c.widths_defaulted
        );
        let _ = writeln!(
            s,
            "geometry          {} lanes repaired after offsetting, {} shorter than {} m",
            c.lanes_repaired, c.short_driving_lanes, self.min_useful_lane_m
        );
        if c.ways_clipped > 0 || c.polygons_clipped > 0 {
            let _ = writeln!(
                s,
                "clipped           {} ways cut at the box into {} runs, {} polygons cut",
                c.ways_clipped, c.clipped_runs, c.polygons_clipped
            );
        }
        let _ = writeln!(
            s,
            "connections       {} ({} banned by {} restrictions)",
            c.connections, c.banned_connections, c.restrictions_applied
        );
        let _ = writeln!(
            s,
            "buildings         {} ({} holes); heights {} tagged, {} from parts, {} from \
             levels, {} defaulted",
            c.buildings,
            c.building_holes,
            c.heights_tagged,
            c.heights_from_parts,
            c.heights_from_levels,
            c.heights_defaulted
        );
        let _ = writeln!(
            s,
            "building parts    {} folded into their outline, {} orphan, {} subsurface \
             dropped, {} spires capped",
            c.building_parts_merged,
            c.building_parts_orphan,
            c.buildings_subsurface,
            c.buildings_spire_capped
        );
        let _ = writeln!(
            s,
            "other             {} crossings, {} land-use zones, {} signal nodes",
            c.crossings, c.landuse_zones, c.traffic_signal_nodes
        );
        let _ = writeln!(s, "anomalies         {} in total", self.total_anomalies());
        for (kind, count) in &self.anomalies {
            let samples = self
                .samples
                .get(kind)
                .map(|ids| {
                    ids.iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if samples.is_empty() {
                let _ = writeln!(s, "  {:<26} {}", kind.label(), count);
            } else {
                let _ = writeln!(s, "  {:<26} {:<8} e.g. {}", kind.label(), count, samples);
            }
        }
        if !self.skipped_simplifications.is_empty() {
            let _ = writeln!(
                s,
                "not implemented   {}",
                self.skipped_simplifications.join(", ")
            );
        }
        s
    }

    /// Compares the class defaults that were used against the limits the source states,
    /// and records the verdict (V4/W1).
    ///
    /// Percentile by nearest rank over a sorted copy — comparisons and one integer
    /// division, no transcendental and no hashing, so it is the same number everywhere.
    fn audit_speed_defaults(&mut self, tagged: &mut [f64], defaulted: &[f64]) {
        tagged.sort_by(f64::total_cmp);
        let n = tagged.len();
        let p95 = if n == 0 {
            0.0
        } else {
            tagged[(n * 95).div_ceil(100).max(1) - 1]
        };
        let threshold = p95 * SPEED_AUDIT_FACTOR;
        let far_above = defaulted.iter().filter(|v| **v > threshold).count();
        let total = n + defaulted.len();
        let share = if total == 0 {
            0.0
        } else {
            far_above as f64 / total as f64
        };
        let fired = n >= SPEED_AUDIT_MIN_TAGGED && share >= SPEED_AUDIT_SHARE;
        self.speed_audit = SpeedAudit {
            tagged_lanes: n as u64,
            defaulted_lanes: defaulted.len() as u64,
            tagged_p95_mps: p95,
            threshold_mps: threshold,
            far_above_lanes: far_above as u64,
            far_above_share: share,
            fired,
        };
        if fired {
            self.note(Anomaly::ClassDefaultsAboveTaggedSpeeds, 0);
        }
    }

    /// Records one anomaly against `element`, an OSM element id (`0` when there is none).
    fn note(&mut self, kind: Anomaly, element: i64) {
        *self.anomalies.entry(kind).or_insert(0) += 1;
        let samples = self.samples.entry(kind).or_default();
        if samples.len() < ANOMALY_SAMPLES && element != 0 && !samples.contains(&element) {
            samples.push(element);
        }
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Which network layers to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsmLayers {
    /// Motor-vehicle ways: `motorway`, `trunk`, `primary`, `secondary`, `tertiary`,
    /// `residential`, `unclassified`, `living_street`, `service`, `busway` and the
    /// `_link` forms.
    pub drivable: bool,
    /// Pedestrian ways: `footway`, `path`, `steps`, `pedestrian`, `corridor`,
    /// `bridleway`, `track`.
    pub pedestrian: bool,
    /// `cycleway` ways.
    pub cycle: bool,
    /// `building` and `building:part` ways and multipolygons.
    pub buildings: bool,
    /// `landuse`, `natural=wood` and `leisure=park` areas.
    pub landuse: bool,
    /// `footway=crossing` ways.
    pub crossings: bool,
}

impl Default for OsmLayers {
    /// Everything.
    fn default() -> Self {
        Self {
            drivable: true,
            pedestrian: true,
            cycle: true,
            buildings: true,
            landuse: true,
            crossings: true,
        }
    }
}

impl OsmLayers {
    /// Only the drivable network — the fastest import, and the one a vehicular scenario
    /// needs.
    pub fn roads_only() -> Self {
        Self {
            drivable: true,
            pedestrian: false,
            cycle: false,
            buildings: false,
            landuse: false,
            crossings: false,
        }
    }
}

/// Which of the osm2streets simplifications to run (04-models.md §1.2).
///
/// Only [`OsmSimplifications::collapse_trivial_junctions`] has an implementation. The
/// other two are here so that a scenario can ask for them and be told, in the report and
/// in the provenance, that they did not run — rather than being told nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsmSimplifications {
    /// Merge the two segments that meet at a node where exactly two compatible segments
    /// meet, and delete the junction. **Implemented.**
    pub collapse_trivial_junctions: bool,
    /// Merge the two carriageways of a dual-carriageway street into one. **Not
    /// implemented**; asking for it records a skipped simplification.
    pub merge_dual_carriageways: bool,
    /// Snap a footway or cycletrack that runs parallel to a road onto that road. **Not
    /// implemented**; asking for it records a skipped simplification.
    pub snap_parallel_footways: bool,
}

impl Default for OsmSimplifications {
    fn default() -> Self {
        Self {
            collapse_trivial_junctions: true,
            merge_dual_carriageways: false,
            snap_parallel_footways: false,
        }
    }
}

/// The fixed-time signal plan defaults of 04-models.md §2.3, which this importer
/// synthesises because an OSM extract carries signal *presence* only.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalDefaults {
    /// Target cycle length, seconds. netconvert `--tls.cycle.time`, default 90.
    pub cycle_s: f64,
    /// Green per phase, seconds. netconvert `--tls.green.time`, default 31. The greens
    /// are stretched or squeezed so that the phases fill [`SignalDefaults::cycle_s`].
    pub green_s: f64,
    /// Shortest green a stretched phase may be reduced to, seconds.
    pub min_green_s: f64,
    /// All-red between phases, seconds. netconvert `--tls.allred.time`, default 0.
    pub all_red_s: f64,
    /// Deceleration used for the amber time, m/s². netconvert `--tls.yellow.min-decel`,
    /// default 3.
    pub yellow_min_decel_mps2: f64,
    /// Driver reaction time in the ITE amber formula `y = t + v / (2a)`, seconds.
    pub yellow_reaction_s: f64,
    /// Lower clamp on the computed amber, seconds (FHWA guidance 3–6 s).
    pub yellow_min_s: f64,
    /// Upper clamp on the computed amber, seconds.
    pub yellow_max_s: f64,
    /// Height of a signal lantern above the road surface, metres.
    pub head_height_m: f64,
}

impl Default for SignalDefaults {
    fn default() -> Self {
        Self {
            cycle_s: 90.0,
            green_s: 31.0,
            min_green_s: 5.0,
            all_red_s: 0.0,
            yellow_min_decel_mps2: 3.0,
            yellow_reaction_s: 1.0,
            yellow_min_s: 3.0,
            yellow_max_s: 6.0,
            head_height_m: 5.0,
        }
    }
}

/// How [`OsmOptions::bbox`] is applied to way geometry (V3/V5).
///
/// The old behaviour was a whole-way **keep filter**: a way was imported entire when any
/// one of its nodes was inside the box. That is why the Phase 1 world spanned 9.31 km²
/// against a requested 3.71 km², and why lane 22 was an 814 m street crossing four
/// avenues that were not in the extract at all — the way was kept whole while its cross
/// streets were not, so the lane carried topology that does not exist.
///
/// Clipping is the default, because a truncated street is honest where an over-long one
/// is not. Whichever arm is chosen is recorded in the provenance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "mode")]
#[non_exhaustive]
pub enum BboxClip {
    /// Keep a way whole when any of its nodes is inside the box.
    ///
    /// Nothing is cut, so no synthetic boundary junction appears and the extent may be
    /// far larger than the request. Kept as an explicit arm so that a scenario that
    /// wants the whole of every touching way can say so.
    KeepWhole,
    /// Cut each way's geometry where it crosses the box grown by `margin_m`,
    /// interpolating the crossing point, and split it into the runs that remain inside.
    ///
    /// A run's cut end becomes an ordinary one-arm junction, so a mobility model sees a
    /// street that ends at the edge of the world rather than one that drives through
    /// intersections the extract does not contain.
    Clip {
        /// How far outside the requested box geometry is still kept, metres.
        margin_m: f64,
    },
}

impl Default for BboxClip {
    /// Clip, with a margin of one junction diameter.
    ///
    /// The margin is `2 × MAX_JUNCTION_RADIUS_M` = 50 m so that a junction sitting on the
    /// boundary is still trimmed with its true radius rather than a truncated one; the
    /// junction radius itself comes from the carriageway widths, which the model card
    /// sources. It is a parameter on the card with a calibration plan, not a constant.
    fn default() -> Self {
        BboxClip::Clip {
            margin_m: 2.0 * MAX_JUNCTION_RADIUS_M,
        }
    }
}

impl BboxClip {
    /// The margin this rule keeps outside the requested box, metres (zero for
    /// [`BboxClip::KeepWhole`]).
    pub fn margin_m(self) -> f64 {
        match self {
            BboxClip::KeepWhole => 0.0,
            BboxClip::Clip { margin_m } => margin_m.max(0.0),
        }
    }

    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            BboxClip::KeepWhole => "keep-whole",
            BboxClip::Clip { .. } => "clip",
        }
    }
}

/// What fixed the world's frame — its origin and therefore every metre coordinate in it
/// (V3).
///
/// The project's premise is byte-identical output, and the frame is the one thing every
/// coordinate depends on. Deriving it from the kept geometry made it depend on whichever
/// way happened to overhang the request furthest, so re-fetching the same box at a
/// slightly different radius moved the whole world and changed the content hash. In
/// descending order of reproducibility:
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FrameRule {
    /// The south-west corner of [`OsmOptions::bbox`] — the scenario's own request, and
    /// the only choice that does not depend on the extract at all.
    RequestedBbox,
    /// The south-west corner of the extract's `<bounds>` element, which Overpass writes
    /// with the box that was queried.
    ExtractBounds,
    /// The south-west corner of the imported geometry. The last resort, for a file with
    /// no `<bounds>` imported with no box: the frame then moves if the geometry does.
    ImportedGeometry,
}

impl Default for FrameRule {
    /// The last resort, which is what an import with neither a request nor `<bounds>`
    /// gets.
    fn default() -> Self {
        FrameRule::ImportedGeometry
    }
}

impl FrameRule {
    /// A stable label for the report and the provenance.
    pub const fn label(self) -> &'static str {
        match self {
            FrameRule::RequestedBbox => "requested-bbox",
            FrameRule::ExtractBounds => "extract-bounds",
            FrameRule::ImportedGeometry => "imported-geometry",
        }
    }

    /// The sentence the provenance records beside the origin.
    pub const fn rule(self) -> &'static str {
        match self {
            FrameRule::RequestedBbox => {
                "south-west corner of the requested bounding box (D6, V3): independent of \
                 what the extract happens to contain"
            }
            FrameRule::ExtractBounds => {
                "south-west corner of the extract's declared <bounds> (D6, V3): no box was \
                 requested"
            }
            FrameRule::ImportedGeometry => {
                "south-west corner of the imported geometry (D6): no box was requested and \
                 the extract declares no <bounds>, so the frame moves if the geometry does"
            }
        }
    }
}

/// Everything [`import_osm`] reads.
///
/// [`OsmOptions::import`] carries the options every importer honours ([`ImportOptions`]);
/// the rest is specific to OSM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsmOptions {
    /// The common import options: import date, simplification tolerance, junction join
    /// distance, signal guessing distance, the building height rule and the index sizing.
    pub import: ImportOptions,
    /// Keep only geometry that touches this geodetic box. `None` keeps the whole file.
    ///
    /// How the box is applied to way geometry is [`OsmOptions::bbox_clip`]. Whichever
    /// arm is chosen, **the box also fixes the world frame**: the origin is its
    /// south-west corner, so re-fetching the extract at a slightly different radius
    /// cannot move a single metre coordinate (V3).
    pub bbox: Option<GeoBbox>,
    /// How [`OsmOptions::bbox`] is applied to way geometry.
    pub bbox_clip: BboxClip,
    /// Which layers to import.
    pub layers: OsmLayers,
    /// Which simplifications to run.
    pub simplify: OsmSimplifications,
    /// Which named set of `highway=*` class defaults to fall back on (V4/W1).
    ///
    /// **Required: there is no default.** The fallback speed of a road class is a
    /// jurisdictional fact, and the shipped `sumo-german` values gave West 49th Street a
    /// 100 km/h limit for every caller who did not think about it. [`OsmOptions::validate`]
    /// — and therefore every import — fails with [`WorldError::InvalidParameter`] until a
    /// preset is named, so a world can never carry a jurisdiction nobody chose. The names
    /// are [`HighwayPreset::label`]: `urban-us-nyc` for a US city, `sumo-german` to stay
    /// comparable with `netconvert --osm-files`.
    #[serde(default)]
    pub highway_preset: Option<HighwayPreset>,
    /// The signal plan defaults.
    pub signals: SignalDefaults,
    /// Width of one motor-traffic lane, metres, forcing the same width on every motor
    /// way whatever its class.
    ///
    /// `None`, the default, reads the way's own `width:lanes` or `width` tag and falls
    /// back to the class default of [`CLASS_LANE_WIDTH_M`] — so an avenue and a service
    /// alley no longer get identical geometry (V6/W2). `Some(w)` is the last resort of
    /// the old behaviour: it replaces the class default, but a way that states its own
    /// width still wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_width_m: Option<f64>,
    /// The shortest driving lane a mobility model can be expected to act in, metres.
    ///
    /// Junction trimming leaves slivers on real arterial approaches — 263 driving lanes
    /// under 5 m on the Phase 1 extract, the shortest 0.99 m (V8). They are kept, because
    /// dropping one disconnects whatever is on the other side, and counted as
    /// [`Anomaly::ShortDrivingLane`] so a consumer can filter them.
    pub min_useful_lane_m: f64,
    /// The fraction of a building's `height` that `roof:height` must reach before the
    /// tagged height is taken to be a spire or a mast tip rather than building mass
    /// (V10).
    ///
    /// The comparison is `>=`, not `>`: a roof exactly this fraction of the height is a
    /// value a mapper writes deliberately, and it must not fall between the two arms of
    /// a strict inequality.
    ///
    /// **`TODO: calibrate`**: no source states a threshold. Above it the obstacle height
    /// is cut back to `height - roof:height`, the structural top, and the building is
    /// counted as [`Anomaly::SpireHeightCapped`].
    pub roof_spire_fraction: f64,
    /// How far beyond the kerb line of the crossing road a lane's stop line is set back
    /// at a junction where three or more motor ways meet, metres.
    ///
    /// A lane used to end exactly at the half-width of the widest carriageway: on the kerb
    /// line of the road it meets, in the middle of where the crosswalk is. That put a
    /// right turn from the kerb lane on a path of 2-3 m radius — tighter than any car can
    /// turn (AASHTO *Green Book* 2018 Table 2-2: passenger-car minimum inside radius
    /// 4.4 m), with the vehicle's body sweeping over the corner building. The setback is
    /// the crosswalk plus the stop line ahead of it: MUTCD 2009 §3B.16 puts the stop line
    /// 4 ft (1.2 m) in advance of the crosswalk, and NYC DOT's *Street Design Manual*
    /// (2020) marks crosswalks 10 ft (3.0 m) wide at minimum, so 4.2 m. A place where
    /// only two ways meet is a continuation, not a junction, and keeps no setback.
    pub stop_line_setback_m: f64,
    /// Width of a sidewalk lane, metres.
    pub sidewalk_width_m: f64,
    /// Width of a cycle lane, metres.
    pub cycleway_width_m: f64,
    /// Default width of a painted crossing, metres, when the way has no `width` tag.
    pub crossing_width_m: f64,
    /// How far a crossing way may be from a junction and still belong to it, metres.
    pub crossing_snap_m: f64,
    /// Generate sidewalk lanes from a road's `sidewalk=left|right|both` tag.
    ///
    /// Default `false`: a city that maps its sidewalks as separate `footway` ways — which
    /// is the mapping this importer's pedestrian layer reads — would otherwise get two
    /// sidewalks for every one on the ground. Turn it on for a city that tags sidewalks
    /// on the carriageway instead.
    pub sidewalks_from_tags: bool,
    /// Let bicycles use motor-traffic lanes, unless the way says otherwise.
    pub bicycles_on_roads: bool,
    /// The terrain grid to attach. `None` leaves the world flat at `z = 0`, which is what
    /// Phase 1 does; this is the hook for the DEM importers of 04-models.md §1.4.
    pub terrain: Option<Terrain>,
}

impl Default for OsmOptions {
    fn default() -> Self {
        Self {
            import: ImportOptions::default(),
            bbox: None,
            bbox_clip: BboxClip::default(),
            layers: OsmLayers::default(),
            simplify: OsmSimplifications::default(),
            highway_preset: None,
            signals: SignalDefaults::default(),
            lane_width_m: None,
            min_useful_lane_m: 5.0,
            roof_spire_fraction: 0.5,
            stop_line_setback_m: 4.2,
            sidewalk_width_m: 2.0,
            cycleway_width_m: 1.5,
            crossing_width_m: 4.0,
            crossing_snap_m: 40.0,
            sidewalks_from_tags: false,
            bicycles_on_roads: true,
            terrain: None,
        }
    }
}

impl OsmOptions {
    /// The options with an import date, which no part of the engine may read from a clock.
    #[must_use]
    pub fn imported_at(mut self, when: impl Into<String>) -> Self {
        self.import.imported_at = when.into();
        self
    }

    /// The options restricted to a geodetic box.
    #[must_use]
    pub fn bbox(mut self, bbox: GeoBbox) -> Self {
        self.bbox = Some(bbox);
        self
    }

    /// The options restricted to a set of layers.
    #[must_use]
    pub fn layers(mut self, layers: OsmLayers) -> Self {
        self.layers = layers;
        self
    }

    /// The options with a different simplification set.
    #[must_use]
    pub fn simplify(mut self, simplify: OsmSimplifications) -> Self {
        self.simplify = simplify;
        self
    }

    /// The options with the `highway=*` class-default preset named (V4/W1).
    ///
    /// There is no default preset, so an import that never calls this fails to validate.
    #[must_use]
    pub fn highway_preset(mut self, preset: HighwayPreset) -> Self {
        self.highway_preset = Some(preset);
        self
    }

    /// The options with a different bounding-box clip rule (V3/V5).
    #[must_use]
    pub fn bbox_clip(mut self, clip: BboxClip) -> Self {
        self.bbox_clip = clip;
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
        for (name, v) in [
            ("lane_width_m", self.lane_width_m.unwrap_or(1.0)),
            ("sidewalk_width_m", self.sidewalk_width_m),
            ("cycleway_width_m", self.cycleway_width_m),
            ("crossing_width_m", self.crossing_width_m),
            ("min_useful_lane_m", self.min_useful_lane_m.max(1.0)),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(bad(name, format!("{v} m is not a positive width")));
            }
        }
        if let BboxClip::Clip { margin_m } = self.bbox_clip {
            if !(margin_m.is_finite() && margin_m >= 0.0) {
                return Err(bad(
                    "bbox_clip.margin_m",
                    format!("{margin_m} m is not a non-negative margin"),
                ));
            }
        }
        if !(self.roof_spire_fraction.is_finite()
            && (0.0..=1.0).contains(&self.roof_spire_fraction))
        {
            return Err(bad(
                "roof_spire_fraction",
                format!("{} is not a fraction in [0, 1]", self.roof_spire_fraction),
            ));
        }
        if !(self.signals.cycle_s.is_finite() && self.signals.cycle_s > 0.0) {
            return Err(bad(
                "signals.cycle_s",
                format!("{} s is not a positive cycle", self.signals.cycle_s),
            ));
        }
        if !(self.signals.yellow_min_decel_mps2.is_finite()
            && self.signals.yellow_min_decel_mps2 > 0.0)
        {
            return Err(bad(
                "signals.yellow_min_decel_mps2",
                format!(
                    "{} m/s^2 is not a positive deceleration",
                    self.signals.yellow_min_decel_mps2
                ),
            ));
        }
        // V4/W1: a class default speed is jurisdictional, so it is the caller's to
        // state. Refusing here is the whole fix: the old code defaulted to German
        // free-flow design speeds and handed Midtown 100 km/h side streets to anyone who
        // did not pass an option.
        if self.highway_preset.is_none() {
            let names: Vec<&str> = HighwayPreset::ALL.iter().map(|p| p.label()).collect();
            return Err(bad(
                "highway_preset",
                format!(
                    "no highway=* class-default preset was selected, and there is no \
                     default because a default speed limit is a statement about a \
                     jurisdiction; select one of: {}",
                    names.join(", ")
                ),
            ));
        }
        if self.import.metres_per_level <= 0.0 {
            return Err(bad(
                "metres_per_level",
                format!(
                    "{} m is not a positive storey",
                    self.import.metres_per_level
                ),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stage 1: the raw file
// ---------------------------------------------------------------------------

/// An element's tags, sorted by key so that lookup is a binary search and iteration is
/// deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags(Vec<(String, String)>);

impl Tags {
    /// The value of `key`, or `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .binary_search_by(|(k, _)| k.as_str().cmp(key))
            .ok()
            .map(|i| self.0[i].1.as_str())
    }

    /// True if `key` is present with exactly this value.
    pub fn is(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }

    /// True if `key` is present at all.
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Every tag, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many tags there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True if the element carries no tags.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds a tag; a repeated key keeps the **first** value, which is what every OSM
    /// consumer does with a malformed element.
    fn insert(&mut self, key: String, value: String) {
        match self.0.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(_) => {}
            Err(at) => self.0.insert(at, (key, value)),
        }
    }
}

/// One `<node>`: an id and a position. Tags live in [`OsmFile::node_tags`], because only
/// a few per cent of nodes have any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawNode {
    /// The OSM node id.
    pub id: i64,
    /// Latitude, degrees north.
    pub lat: f64,
    /// Longitude, degrees east.
    pub lon: f64,
}

/// One `<way>`: an id, its node references in order, and its tags.
#[derive(Debug, Clone, PartialEq)]
pub struct RawWay {
    /// The OSM way id.
    pub id: i64,
    /// Its `<nd ref>` references, in order.
    pub nodes: Vec<i64>,
    /// Its tags.
    pub tags: Tags,
}

/// What an OSM relation member refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberKind {
    /// A node.
    Node,
    /// A way.
    Way,
    /// Another relation (this importer does not recurse into them).
    Relation,
}

/// One `<member>` of a relation.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// What it refers to.
    pub kind: MemberKind,
    /// The referenced element's id.
    pub id: i64,
    /// The member's role, e.g. `outer`, `inner`, `from`, `via`, `to`.
    pub role: String,
}

/// One `<relation>`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRelation {
    /// The OSM relation id.
    pub id: i64,
    /// Its members, in file order (which is significant for multipolygon rings).
    pub members: Vec<Member>,
    /// Its tags.
    pub tags: Tags,
}

/// A parsed OSM XML file: nodes, ways and relations, each sorted by id.
///
/// Sorting is what makes the import deterministic: everything downstream iterates these
/// vectors in id order, so no hash-map iteration and no file ordering can reach an output
/// (crate rule 2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsmFile {
    /// The `<bounds>` element, when the extract carries one.
    pub bounds: Option<GeoBbox>,
    /// The text of an Overpass `<remark>` element, when the document carries one.
    ///
    /// Overpass reports a timeout or an out-of-memory failure as a well-formed `<osm>`
    /// document with a `<remark>` and no data. Keeping the text is what lets the importer
    /// refuse it with the server's own words rather than import a content-addressed empty
    /// world (R8).
    pub remark: Option<String>,
    /// Every node, sorted by id.
    pub nodes: Vec<RawNode>,
    /// The tags of the nodes that have any, by node id.
    pub node_tags: BTreeMap<i64, Tags>,
    /// Every way, sorted by id.
    pub ways: Vec<RawWay>,
    /// Every relation, sorted by id.
    pub relations: Vec<RawRelation>,
}

impl OsmFile {
    /// The index of node `id` in [`OsmFile::nodes`], or `None`.
    pub fn node_index(&self, id: i64) -> Option<usize> {
        self.nodes.binary_search_by_key(&id, |n| n.id).ok()
    }

    /// Node `id`, or `None`.
    pub fn node(&self, id: i64) -> Option<&RawNode> {
        self.node_index(id).map(|i| &self.nodes[i])
    }

    /// The index of way `id` in [`OsmFile::ways`], or `None`.
    pub fn way_index(&self, id: i64) -> Option<usize> {
        self.ways.binary_search_by_key(&id, |w| w.id).ok()
    }

    /// Way `id`, or `None`.
    pub fn way(&self, id: i64) -> Option<&RawWay> {
        self.way_index(id).map(|i| &self.ways[i])
    }

    /// The tags of node `id` — an empty set when it has none.
    pub fn tags_of_node(&self, id: i64) -> Option<&Tags> {
        self.node_tags.get(&id)
    }
}

/// The element the streaming parser is in the middle of.
enum Pending {
    Node(RawNode, Tags),
    Way(RawWay),
    Relation(RawRelation),
}

/// Parses an OSM XML document into an [`OsmFile`].
///
/// One streaming pass with `quick-xml`: no DOM, no intermediate allocation per element
/// beyond the element itself. Unknown elements and attributes are ignored, as the OSM XML
/// schema requires of a reader; `<node>`s with unparsable coordinates are dropped.
///
/// # Errors
///
/// [`WorldError::Malformed`] if the XML itself does not parse, **or if its root element is
/// not `<osm>`**. The second check is what stops an HTML `502 Bad Gateway` page or any
/// other well-formed document from importing as a valid, empty, content-addressed world
/// (R8): a failed download must not be indistinguishable from an empty bounding box.
/// Bad *data* inside a real OSM document is never an error here: it is counted by the
/// caller.
pub fn parse_osm(xml: &[u8]) -> Result<OsmFile> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut file = OsmFile::default();
    let mut pending: Option<Pending> = None;
    let mut buf = Vec::new();
    let mut saw_osm_root = false;
    let mut in_remark = false;

    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| {
            let offset = reader.buffer_position() as usize;
            WorldError::Malformed {
                offset,
                problem: format!("osm xml: {e}"),
            }
        })?;
        match event {
            quick_xml::events::Event::Eof => break,
            quick_xml::events::Event::Start(ref e) | quick_xml::events::Event::Empty(ref e) => {
                let empty = matches!(event, quick_xml::events::Event::Empty(_));
                let name = e.name().into_inner().to_string();
                match name.as_str() {
                    "osm" => saw_osm_root = true,
                    "remark" => in_remark = !empty,
                    "bounds" => {
                        let minlat = attr_f64(e, "minlat");
                        let minlon = attr_f64(e, "minlon");
                        let maxlat = attr_f64(e, "maxlat");
                        let maxlon = attr_f64(e, "maxlon");
                        if let (Some(a), Some(b), Some(c), Some(d)) =
                            (minlat, minlon, maxlat, maxlon)
                        {
                            file.bounds = Some(GeoBbox::new(a, b, c, d));
                        }
                    }
                    "node" => {
                        let (id, lat, lon) =
                            (attr_i64(e, "id"), attr_f64(e, "lat"), attr_f64(e, "lon"));
                        if let (Some(id), Some(lat), Some(lon)) = (id, lat, lon) {
                            let node = RawNode { id, lat, lon };
                            if empty {
                                file.nodes.push(node);
                            } else {
                                pending = Some(Pending::Node(node, Tags::default()));
                            }
                        }
                    }
                    "way" => {
                        if let Some(id) = attr_i64(e, "id") {
                            let way = RawWay {
                                id,
                                nodes: Vec::new(),
                                tags: Tags::default(),
                            };
                            if empty {
                                file.ways.push(way);
                            } else {
                                pending = Some(Pending::Way(way));
                            }
                        }
                    }
                    "relation" => {
                        if let Some(id) = attr_i64(e, "id") {
                            let relation = RawRelation {
                                id,
                                members: Vec::new(),
                                tags: Tags::default(),
                            };
                            if empty {
                                file.relations.push(relation);
                            } else {
                                pending = Some(Pending::Relation(relation));
                            }
                        }
                    }
                    "nd" => {
                        if let (Some(Pending::Way(way)), Some(r)) =
                            (pending.as_mut(), attr_i64(e, "ref"))
                        {
                            way.nodes.push(r);
                        }
                    }
                    "member" => {
                        if let Some(Pending::Relation(relation)) = pending.as_mut() {
                            let kind = match attr_str(e, "type").as_deref() {
                                Some("node") => Some(MemberKind::Node),
                                Some("way") => Some(MemberKind::Way),
                                Some("relation") => Some(MemberKind::Relation),
                                _ => None,
                            };
                            if let (Some(kind), Some(id)) = (kind, attr_i64(e, "ref")) {
                                relation.members.push(Member {
                                    kind,
                                    id,
                                    role: attr_str(e, "role").unwrap_or_default(),
                                });
                            }
                        }
                    }
                    "tag" => {
                        if let (Some(k), Some(v)) = (attr_str(e, "k"), attr_str(e, "v")) {
                            match pending.as_mut() {
                                Some(Pending::Node(_, tags)) => tags.insert(k, v),
                                Some(Pending::Way(way)) => way.tags.insert(k, v),
                                Some(Pending::Relation(relation)) => relation.tags.insert(k, v),
                                None => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            quick_xml::events::Event::Text(ref t) if in_remark => {
                let raw = t.xml_content(quick_xml::XmlVersion::Implicit1_0);
                let text = quick_xml::escape::unescape(&raw)
                    .map(|t| t.trim().to_string())
                    .unwrap_or_else(|_| raw.trim().to_string());
                if !text.is_empty() {
                    file.remark = Some(text);
                }
            }
            quick_xml::events::Event::End(ref e) => {
                let name = e.name().into_inner();
                if name == "remark" {
                    in_remark = false;
                }
                if matches!(name, "node" | "way" | "relation") {
                    match pending.take() {
                        Some(Pending::Node(node, tags)) => {
                            if !tags.is_empty() {
                                file.node_tags.insert(node.id, tags);
                            }
                            file.nodes.push(node);
                        }
                        Some(Pending::Way(way)) => file.ways.push(way),
                        Some(Pending::Relation(relation)) => file.relations.push(relation),
                        None => {}
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    // Sorting by id is what makes every later stage's iteration order independent of the
    // file's. A duplicate id keeps the first element, as OSM's own readers do.
    file.nodes.sort_by_key(|n| n.id);
    file.nodes.dedup_by_key(|n| n.id);
    file.ways.sort_by_key(|w| w.id);
    file.ways.dedup_by_key(|w| w.id);
    file.relations.sort_by_key(|r| r.id);
    file.relations.dedup_by_key(|r| r.id);
    if !saw_osm_root {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: "not an OpenStreetMap document: no <osm> root element".to_string(),
        });
    }
    Ok(file)
}

/// An attribute's value, unescaped.
fn attr_str(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
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

/// An attribute parsed as an integer.
fn attr_i64(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<i64> {
    attr_str(e, key)?.trim().parse::<i64>().ok()
}

/// An attribute parsed as a finite float.
fn attr_f64(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<f64> {
    let v = attr_str(e, key)?.trim().parse::<f64>().ok()?;
    v.is_finite().then_some(v)
}

// ---------------------------------------------------------------------------
// Stage 2: tag parsing
// ---------------------------------------------------------------------------

/// Which routable family a way belongs to.
///
/// The families are kept apart because they are split at different nodes: a crosswalk
/// touching a street must not cut that street into two edges, and a street touching a
/// footway must not cut the footway either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WayFamily {
    /// A carriageway motor vehicles drive on.
    Motor,
    /// A cycleway.
    Cycle,
    /// A footway, path, steps or pedestrian street.
    Foot,
}

impl WayFamily {
    /// The lane kind a way of this family produces.
    const fn lane_kind(self) -> LaneKind {
        match self {
            WayFamily::Motor => LaneKind::Driving,
            WayFamily::Cycle => LaneKind::Cycle,
            WayFamily::Foot => LaneKind::Sidewalk,
        }
    }
}

/// One row of a `highway=*` default table: what one tag value becomes when the way
/// carries no explicit `lanes`, `maxspeed` or `width`.
///
/// Every row names the source of its speed, because a class default speed is
/// jurisdiction-specific and silently shipping one is exactly what the project's
/// no-black-box rule forbids (V4/W1).
struct HighwayDefault {
    /// The tag value.
    key: &'static str,
    /// The functional class it maps to.
    class: RoadClass,
    /// Which family it belongs to.
    family: WayFamily,
    /// Lanes per direction when the way carries no `lanes*` tag.
    lanes: u8,
    /// Speed limit when the way carries no usable `maxspeed`, m/s.
    speed_mps: f64,
    /// Where `speed_mps` comes from — a citation, not a description.
    speed_source: &'static str,
    /// Whether the value implies one-way without an `oneway` tag.
    implicit_oneway: bool,
}

/// A named, swappable set of `highway=*` class defaults (V4/W1).
///
/// The default speeds an OSM importer falls back to are **jurisdictional**, not
/// universal: SUMO's type map carries German design speeds, which give a Midtown
/// Manhattan side street a 100 km/h limit. Making the table a named preset that the
/// scenario selects, and recording the name in the report and the provenance, is what
/// turns a silent wrong number into a stated choice.
///
/// Ways that carry a `maxspeed` tag are unaffected: the preset supplies the *fallback*
/// only, and [`ImportCounts::speeds_defaulted`] counts how often it was reached.
///
/// There is deliberately **no [`Default`]**. A class default speed is a statement about
/// a jurisdiction, and no jurisdiction is a safe guess: whichever preset were the
/// default would silently be asserted about every extract that did not choose. The
/// scenario states one, and [`OsmOptions::validate`] refuses the import until it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum HighwayPreset {
    /// SUMO `netconvert`'s own OSM type map: German free-flow **design** speeds
    /// (motorway 39.44 m/s, trunk/primary/secondary 27.78 m/s).
    ///
    /// Kept so that this importer and a `netconvert --osm-files` import of the same
    /// extract stay comparable (04-models.md §1.2 names netconvert as the delegate
    /// path). It is **not** an urban legal limit: on the Phase 1 Manhattan extract it
    /// gives 404 driving lanes a limit above 90 km/h, 393 of them at 100.01 km/h on
    /// named Midtown side streets. It is **not** the default any more, and there is no
    /// default — see [`OsmOptions::highway_preset`].
    SumoGerman,
    /// New York City legal defaults: the citywide 25 mph default on every street, and
    /// the New York State statutory maximum of 55 mph on a controlled-access motorway.
    ///
    /// The class distinction almost disappears on purpose — the law makes none. A street
    /// with a different posted limit carries `maxspeed` in OSM, and the tag always wins.
    UrbanUsNyc,
}

impl HighwayPreset {
    /// Every preset, in a stable order.
    pub const ALL: [HighwayPreset; 2] = [HighwayPreset::SumoGerman, HighwayPreset::UrbanUsNyc];

    /// The preset's stable kebab-case name, as the report, the provenance and a scenario
    /// file spell it.
    pub const fn label(self) -> &'static str {
        match self {
            HighwayPreset::SumoGerman => "sumo-german",
            HighwayPreset::UrbanUsNyc => "urban-us-nyc",
        }
    }

    /// The preset named `name`, or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.label() == name)
    }

    /// Where this preset's numbers come from, for the model card and the provenance.
    pub const fn source(self) -> &'static str {
        match self {
            HighwayPreset::SumoGerman => {
                "SUMO netconvert osmNetconvert.typ.xml (German design speeds), via \
                 04-models.md §1.2"
            }
            HighwayPreset::UrbanUsNyc => {
                "New York City citywide 25 mph default speed limit (NYC Local Law 139 of \
                 2014, under NY Vehicle & Traffic Law §1643) and the NY State statutory \
                 maximum of 55 mph (NY VTL §1180(b))"
            }
        }
    }

    /// The table this preset selects.
    const fn table(self) -> &'static [HighwayDefault] {
        match self {
            HighwayPreset::SumoGerman => SUMO_GERMAN_TABLE,
            HighwayPreset::UrbanUsNyc => URBAN_US_NYC_TABLE,
        }
    }

    /// The row for a `highway=*` value, or `None` if this importer does not model it
    /// (`construction`, `proposed`, `raceway`, `platform`, …).
    fn row(self, value: &str) -> Option<&'static HighwayDefault> {
        self.table().iter().find(|d| d.key == value)
    }

    /// Every distinct citation the preset's rows carry, in table order.
    ///
    /// "A cited source per row" is the requirement; a preset's rows share a handful of
    /// citations, so this is the list that goes on the model card and into the
    /// provenance without repeating one twenty times.
    pub fn row_sources(self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for row in self.table() {
            if !out.contains(&row.speed_source) {
                out.push(row.speed_source);
            }
        }
        out
    }
}

/// Where the pedestrian and cycle rows' "speed limits" come from.
///
/// They are walking and riding paces, not legal limits, so they are the same in every
/// preset: a jurisdiction does not post a speed limit on a staircase.
const PACE_SOURCE: &str = "walking and cycling pace convention of 04-models.md §2.5, not \
                           a posted limit (5 km/h on the flat, 0.5 m/s on steps)";

/// `sumo-german`: the type map `netconvert --osm-files` uses by default.
///
/// The values are reproduced from SUMO's `osmNetconvert.typ.xml` so that this importer
/// and a netconvert-based import of the same extract produce comparable networks. They
/// are **German free-flow design speeds** and are marked `todo-calibrate` on the model
/// card: the file was not re-opened during this build, and 04-models.md §1.2's plan is to
/// replace the speeds with the measured `maxspeed` distribution of the Phase 2 city
/// boxes.
#[rustfmt::skip]
const SUMO_GERMAN_TABLE: &[HighwayDefault] = &[
    // --- motor traffic ------------------------------------------------------
    HighwayDefault { key: "motorway",       class: RoadClass::Motorway,     family: WayFamily::Motor, lanes: 2, speed_mps: 39.44, speed_source: SUMO_TYP, implicit_oneway: true },
    HighwayDefault { key: "motorway_link",  class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: true },
    HighwayDefault { key: "trunk",          class: RoadClass::Trunk,        family: WayFamily::Motor, lanes: 2, speed_mps: 27.78, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "trunk_link",     class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "primary",        class: RoadClass::Primary,      family: WayFamily::Motor, lanes: 2, speed_mps: 27.78, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "primary_link",   class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "secondary",      class: RoadClass::Secondary,    family: WayFamily::Motor, lanes: 2, speed_mps: 27.78, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "secondary_link", class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "tertiary",       class: RoadClass::Tertiary,     family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "tertiary_link",  class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 22.22, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "unclassified",   class: RoadClass::Unclassified, family: WayFamily::Motor, lanes: 1, speed_mps: 13.89, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "residential",    class: RoadClass::Residential,  family: WayFamily::Motor, lanes: 1, speed_mps: 13.89, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "living_street",  class: RoadClass::Living,       family: WayFamily::Motor, lanes: 1, speed_mps: 2.78,  speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "service",        class: RoadClass::Service,      family: WayFamily::Motor, lanes: 1, speed_mps: 5.56,  speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "busway",         class: RoadClass::Service,      family: WayFamily::Motor, lanes: 1, speed_mps: 13.89, speed_source: SUMO_TYP, implicit_oneway: false },
    HighwayDefault { key: "road",           class: RoadClass::Unclassified, family: WayFamily::Motor, lanes: 1, speed_mps: 13.89, speed_source: SUMO_TYP, implicit_oneway: false },
    // --- cycle --------------------------------------------------------------
    HighwayDefault { key: "cycleway",       class: RoadClass::Cycleway,     family: WayFamily::Cycle, lanes: 1, speed_mps: 5.56,  speed_source: SUMO_TYP, implicit_oneway: false },
    // --- foot ---------------------------------------------------------------
    HighwayDefault { key: "footway",        class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "pedestrian",     class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "corridor",       class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "path",           class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "track",          class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "bridleway",      class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "steps",          class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 0.5,   speed_source: PACE_SOURCE, implicit_oneway: false },
];

/// The citation every `sumo-german` motor row carries.
const SUMO_TYP: &str = "SUMO netconvert osmNetconvert.typ.xml, via 04-models.md §1.2 \
                        (German design speed, UNVERIFIED)";

/// The citation every 25 mph row of `urban-us-nyc` carries.
const NYC_25: &str = "New York City citywide default speed limit, 25 mph = 11.176 m/s \
                      (NYC Local Law 139 of 2014, under NY VTL §1643)";

/// The citation the motorway row of `urban-us-nyc` carries.
const NY_55: &str = "New York State statutory maximum speed limit, 55 mph = 24.587 m/s \
                     (NY VTL §1180(b)); a controlled-access highway in the city is always \
                     posted, so an untagged one falls back to the statutory maximum";

/// `urban-us-nyc`: the legal defaults of a US city that has set a citywide limit.
///
/// Lane counts are unchanged from [`SUMO_GERMAN_TABLE`] — a carriageway's lane count is
/// geometry, not jurisdiction — and the pedestrian and cycle paces are unchanged for the
/// same reason. Only the motor speeds differ, and each one cites the statute it comes
/// from.
#[rustfmt::skip]
const URBAN_US_NYC_TABLE: &[HighwayDefault] = &[
    // --- motor traffic ------------------------------------------------------
    HighwayDefault { key: "motorway",       class: RoadClass::Motorway,     family: WayFamily::Motor, lanes: 2, speed_mps: 24.587, speed_source: NY_55, implicit_oneway: true },
    HighwayDefault { key: "motorway_link",  class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: true },
    HighwayDefault { key: "trunk",          class: RoadClass::Trunk,        family: WayFamily::Motor, lanes: 2, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "trunk_link",     class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "primary",        class: RoadClass::Primary,      family: WayFamily::Motor, lanes: 2, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "primary_link",   class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "secondary",      class: RoadClass::Secondary,    family: WayFamily::Motor, lanes: 2, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "secondary_link", class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "tertiary",       class: RoadClass::Tertiary,     family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "tertiary_link",  class: RoadClass::Link,         family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "unclassified",   class: RoadClass::Unclassified, family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "residential",    class: RoadClass::Residential,  family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "living_street",  class: RoadClass::Living,       family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "service",        class: RoadClass::Service,      family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "busway",         class: RoadClass::Service,      family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    HighwayDefault { key: "road",           class: RoadClass::Unclassified, family: WayFamily::Motor, lanes: 1, speed_mps: 11.176, speed_source: NYC_25, implicit_oneway: false },
    // --- cycle --------------------------------------------------------------
    HighwayDefault { key: "cycleway",       class: RoadClass::Cycleway,     family: WayFamily::Cycle, lanes: 1, speed_mps: 5.56,  speed_source: PACE_SOURCE, implicit_oneway: false },
    // --- foot ---------------------------------------------------------------
    HighwayDefault { key: "footway",        class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "pedestrian",     class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "corridor",       class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "path",           class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "track",          class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "bridleway",      class: RoadClass::Path,         family: WayFamily::Foot,  lanes: 1, speed_mps: 1.39,  speed_source: PACE_SOURCE, implicit_oneway: false },
    HighwayDefault { key: "steps",          class: RoadClass::Footway,      family: WayFamily::Foot,  lanes: 1, speed_mps: 0.5,   speed_source: PACE_SOURCE, implicit_oneway: false },
];

/// The class-based carriageway lane width, metres, and its source (V6/W2).
///
/// Reached only when the way carries neither `width:lanes` nor `width`. The widths are
/// the geometric-design values for the class, not a single global constant: an 18 m
/// avenue and a 6 m service alley must not get identical per-lane geometry.
///
/// Sources: AASHTO *A Policy on Geometric Design of Highways and Streets* gives lane
/// widths of 9 to 12 ft, with 12 ft (3.658 m) on freeways and major arterials and 10 to
/// 11 ft acceptable on collectors and local streets; NACTO's *Urban Street Design Guide*
/// recommends 10 ft (3.048 m) for an urban street lane and 11 ft (3.353 m) where a
/// wider lane is warranted. The assignment of a class to a width inside those ranges is
/// this importer's, and the parameter is on the model card with both sources.
#[rustfmt::skip]
const CLASS_LANE_WIDTH_M: &[(RoadClass, f64)] = &[
    (RoadClass::Motorway,     3.658), // 12 ft, AASHTO freeway
    (RoadClass::Trunk,        3.658), // 12 ft, AASHTO major arterial
    (RoadClass::Link,         3.353), // 11 ft, AASHTO ramp
    (RoadClass::Primary,      3.353), // 11 ft, NACTO urban arterial
    (RoadClass::Secondary,    3.353), // 11 ft, NACTO urban arterial
    (RoadClass::Tertiary,     3.048), // 10 ft, NACTO urban street
    (RoadClass::Unclassified, 3.048), // 10 ft, NACTO urban street
    (RoadClass::Residential,  3.048), // 10 ft, NACTO urban street
    (RoadClass::Living,       2.743), //  9 ft, AASHTO low-volume local
    (RoadClass::Service,      2.743), //  9 ft, AASHTO low-volume local
];

/// The source every row of [`CLASS_LANE_WIDTH_M`] cites.
const LANE_WIDTH_SOURCE: &str = "AASHTO Green Book (9-12 ft lanes; 12 ft on freeways and \
                                 major arterials) and NACTO Urban Street Design Guide \
                                 (10-11 ft urban street lanes)";

/// The class-based lane width for `class`, or `None` for a class that carries no motor
/// traffic.
fn class_lane_width_m(class: RoadClass) -> Option<f64> {
    CLASS_LANE_WIDTH_M
        .iter()
        .find(|(c, _)| *c == class)
        .map(|(_, w)| *w)
}

/// Where a way's lane width came from (V6/W2), so the report can say how many lanes are
/// still carrying a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidthSource {
    /// A `width:lanes` or `lanes:width` tag.
    PerLaneTag,
    /// A `width` tag, divided by the lane count.
    WayTag,
    /// The caller's global [`OsmOptions::lane_width_m`] override.
    Option,
    /// [`CLASS_LANE_WIDTH_M`].
    Class,
}

/// The narrowest lane width this importer will believe, metres.
///
/// A passenger car of EU category M1 is at most 2.0 m wide excluding mirrors, so a
/// carriageway lane narrower than this cannot carry one and the tag is a mistake (a
/// `width` given for one side of a shared space, or in the wrong unit).
const MIN_LANE_WIDTH_M: f64 = 1.5;

/// The widest lane width this importer will believe, metres — wider than any lane on any
/// urban carriageway, so a `width=100` or a `width` that is really the whole right of way
/// is rejected rather than turned into geometry.
const MAX_LANE_WIDTH_M: f64 = 8.0;

/// The last-resort lane width, metres, for a motor class with no row in
/// [`CLASS_LANE_WIDTH_M`]. Every class this importer maps to [`WayFamily::Motor`] has a
/// row, so this is unreachable today; it is the historical `lane_width_m` default and is
/// here so that adding a motor class cannot silently produce a zero-width lane.
const FALLBACK_LANE_WIDTH_M: f64 = 3.5;

/// One motor way's lane width, and where it came from (V6/W2).
///
/// In order: `width:lanes` (or the `lanes:width` spelling that occurs in the wild), then
/// `width` divided by the total lane count, then the caller's global
/// [`OsmOptions::lane_width_m`] override when it is set, then the class default of
/// [`CLASS_LANE_WIDTH_M`]. A tag outside `[MIN_LANE_WIDTH_M, MAX_LANE_WIDTH_M]` is
/// counted as unparsable and ignored, because an 18 m avenue and a 6 m alley must differ
/// but a 100 m lane is a typo.
fn motor_lane_width(
    tags: &Tags,
    class: RoadClass,
    total_lanes: u8,
    osm_id: i64,
    options: &OsmOptions,
    report: &mut ImportReport,
) -> (f64, WidthSource) {
    let plausible = |w: f64| w.is_finite() && (MIN_LANE_WIDTH_M..=MAX_LANE_WIDTH_M).contains(&w);
    // `width:lanes=3|3.5|3` gives one width per lane; the model holds one width per way,
    // so the mean is taken and the per-lane detail recorded as ignored on the card.
    for key in ["width:lanes", "lanes:width"] {
        let Some(raw) = tags.get(key) else { continue };
        let mut sum = 0.0f64;
        let mut used = 0u32;
        let mut rejected = false;
        for token in raw.split('|') {
            if token.trim().is_empty() {
                continue;
            }
            match parse_measure(token) {
                Some(measure) if plausible(measure.metres) => {
                    sum += measure.metres;
                    used += 1;
                }
                _ => rejected = true,
            }
        }
        if rejected {
            report.note(Anomaly::UnparsableHeight, osm_id);
        }
        if used > 0 {
            return (sum / f64::from(used), WidthSource::PerLaneTag);
        }
    }
    if let Some(raw) = tags.get("width") {
        match parse_measure(raw) {
            Some(measure) => {
                if measure.multi_valued {
                    report.note(Anomaly::MultiValuedHeight, osm_id);
                }
                let per_lane = measure.metres / f64::from(total_lanes.max(1));
                if plausible(per_lane) {
                    return (per_lane, WidthSource::WayTag);
                }
                report.note(Anomaly::UnparsableHeight, osm_id);
            }
            None => report.note(Anomaly::UnparsableHeight, osm_id),
        }
    }
    if let Some(forced) = options.lane_width_m {
        return (forced, WidthSource::Option);
    }
    (
        class_lane_width_m(class).unwrap_or(FALLBACK_LANE_WIDTH_M),
        WidthSource::Class,
    )
}

/// What a `maxspeed` value turned out to be.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Maxspeed {
    /// A usable limit, m/s.
    Mps(f64),
    /// `none`: an explicitly unlimited road.
    Unlimited,
    /// Something no rule could parse.
    Unparsable,
    /// No tag at all.
    Absent,
}

/// Parses an OSM `maxspeed` value.
///
/// Accepted: a bare number in km/h (`50`), `N mph`, `N knots`, `walk` (5 km/h), and
/// `none`. A conditional or zone value (`RU:urban`, `signals`, `variable`) is
/// [`Maxspeed::Unparsable`], as is a negative or non-finite number. A multi-valued
/// `50;30` takes the first value, because a single limit is all the model holds.
fn parse_maxspeed(value: &str) -> Maxspeed {
    let raw = value.trim();
    if raw.is_empty() {
        return Maxspeed::Absent;
    }
    let first = raw.split(';').next().unwrap_or(raw).trim();
    let lower = first.to_ascii_lowercase();
    if lower == "none" {
        return Maxspeed::Unlimited;
    }
    if lower == "walk" {
        return Maxspeed::Mps(5.0 / 3.6);
    }
    let (number, factor) = if let Some(rest) = lower.strip_suffix("mph") {
        (rest, 1.609_344 / 3.6)
    } else if let Some(rest) = lower.strip_suffix("knots") {
        (rest, 1.852 / 3.6)
    } else if let Some(rest) = lower.strip_suffix("km/h") {
        (rest, 1.0 / 3.6)
    } else if let Some(rest) = lower.strip_suffix("kmh") {
        (rest, 1.0 / 3.6)
    } else {
        (lower.as_str(), 1.0 / 3.6)
    };
    match number.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Maxspeed::Mps(v * factor),
        _ => Maxspeed::Unparsable,
    }
}

/// What an `oneway` value turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Oneway {
    /// Traffic runs in the way's own direction only.
    Forward,
    /// Traffic runs against the way's direction only (`oneway=-1`).
    Backward,
    /// Both directions.
    Both,
    /// `reversible` or `alternating`: direction varies with time.
    Reversible,
    /// Something no rule could parse.
    Unparsable,
    /// No tag at all.
    Absent,
}

/// Parses an OSM `oneway` value.
fn parse_oneway(value: &str) -> Oneway {
    match value.trim().to_ascii_lowercase().as_str() {
        "" => Oneway::Absent,
        "yes" | "true" | "1" => Oneway::Forward,
        "-1" | "reverse" => Oneway::Backward,
        "no" | "false" | "0" => Oneway::Both,
        "reversible" | "alternating" => Oneway::Reversible,
        _ => Oneway::Unparsable,
    }
}

/// Parses a non-negative integer count (`lanes`, `lanes:forward`, `building:levels`).
fn parse_count(value: &str) -> Option<u32> {
    let first = value.split(';').next()?.trim();
    // `2.0` appears in the wild for a lane count and for a storey count.
    let parsed = first.parse::<f64>().ok()?;
    if !parsed.is_finite() || parsed < 0.0 || parsed > 1e6 {
        return None;
    }
    Some(parsed.round() as u32)
}

/// What a length-valued tag (`height`, `min_height`, `width`) turned out to be.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measure {
    /// The value in metres.
    metres: f64,
    /// True if the tag held several values and the first was taken.
    multi_valued: bool,
}

/// Parses an OSM length tag into metres.
///
/// Accepted: a bare number in metres, `N m`, `N ft`, `N'` and `N' M"` (feet and inches),
/// `N cm`. `12;15` takes the first value and flags it. Anything else is `None`.
///
/// The imperial forms are the ones the OSM Simple 3D Buildings page documents for
/// `height`; ignoring them would silently import a 30-storey building as 100 metres or as
/// nothing at all.
fn parse_measure(value: &str) -> Option<Measure> {
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }
    let multi_valued = raw.contains(';');
    let first = raw.split(';').next().unwrap_or(raw).trim();
    let lower = first.to_ascii_lowercase();

    // Feet and inches: `12'6"` or `12'`.
    if let Some((feet, rest)) = lower.split_once('\'') {
        let feet: f64 = feet.trim().parse().ok()?;
        let inches: f64 = {
            let r = rest.trim().trim_end_matches('"').trim();
            if r.is_empty() { 0.0 } else { r.parse().ok()? }
        };
        let metres = feet * 0.3048 + inches * 0.0254;
        return metres.is_finite().then_some(Measure {
            metres,
            multi_valued,
        });
    }

    let (number, factor) = if let Some(rest) = lower.strip_suffix("meters") {
        (rest, 1.0)
    } else if let Some(rest) = lower.strip_suffix("metres") {
        (rest, 1.0)
    } else if let Some(rest) = lower.strip_suffix("cm") {
        (rest, 0.01)
    } else if let Some(rest) = lower.strip_suffix("mm") {
        (rest, 0.001)
    } else if let Some(rest) = lower.strip_suffix("km") {
        (rest, 1000.0)
    } else if let Some(rest) = lower.strip_suffix("ft") {
        (rest, 0.3048)
    } else if let Some(rest) = lower.strip_suffix("feet") {
        (rest, 0.3048)
    } else if let Some(rest) = lower.strip_suffix('m') {
        (rest, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let parsed: f64 = number.trim().parse().ok()?;
    let metres = parsed * factor;
    metres.is_finite().then_some(Measure {
        metres,
        multi_valued,
    })
}

/// Parses one `turn:lanes` token set, e.g. `through;right`.
///
/// Returns an empty vector for `none`, for the empty token and for an unknown token,
/// which the caller reads as "this lane is not restricted".
fn parse_turn_token(token: &str) -> core::result::Result<Vec<TurnDirection>, ()> {
    let mut out = Vec::new();
    let token = token.trim();
    if token.is_empty() || token == "none" {
        return Ok(out);
    }
    for part in token.split(';') {
        let turn = match part.trim() {
            "" | "none" => continue,
            "through" => TurnDirection::Straight,
            "left" => TurnDirection::Left,
            "right" => TurnDirection::Right,
            "slight_left" => TurnDirection::SlightLeft,
            "slight_right" => TurnDirection::SlightRight,
            "sharp_left" => TurnDirection::Left,
            "sharp_right" => TurnDirection::Right,
            "reverse" => TurnDirection::UTurn,
            // A merge is a through movement that changes lane; the model has no separate
            // code for it, and treating it as a turn restriction would ban going straight.
            "merge_to_left" | "merge_to_right" => TurnDirection::Straight,
            _ => return Err(()),
        };
        if !out.contains(&turn) {
            out.push(turn);
        }
    }
    Ok(out)
}

/// Parses a whole `turn:lanes` value into one token set per lane, **leftmost lane first**
/// (the OSM order).
///
/// `left|through|through;right` becomes three sets. An unknown token anywhere rejects the
/// whole value, because a partly understood turn assignment is worse than an inferred one.
fn parse_turn_lanes(value: &str) -> Option<Vec<Vec<TurnDirection>>> {
    let mut out = Vec::new();
    for token in value.split('|') {
        out.push(parse_turn_token(token).ok()?);
    }
    (!out.is_empty()).then_some(out)
}

/// True if a value means "yes" in the OSM access vocabulary.
fn access_permits(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "designated" | "permissive" | "destination" | "official" | "customers" | "delivery"
    )
}

/// True if a value means "no".
fn access_denies(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "no" | "private" | "agricultural" | "forestry" | "military" | "emergency"
    )
}

// ---------------------------------------------------------------------------
// Stage 2 (continued): what one way becomes
// ---------------------------------------------------------------------------

/// The importer's plan for one OSM way: what the tags say it is, in world terms.
///
/// Built once per way by [`classify_way`], then shared by every segment that way is cut
/// into. Lane counts are **per direction**: `lanes_fwd` lanes run along the way's own node
/// order, `lanes_bwd` against it.
#[derive(Debug, Clone, PartialEq)]
pub struct WayPlan {
    /// Index into [`OsmFile::ways`].
    pub way: usize,
    /// The OSM way id, for the report and for id ordering.
    pub osm_id: i64,
    /// Which network this way belongs to.
    pub family: WayFamily,
    /// Its functional class.
    pub class: RoadClass,
    /// Lanes running along the way's node order.
    pub lanes_fwd: u8,
    /// Lanes running against it.
    pub lanes_bwd: u8,
    /// Speed limit, m/s.
    pub speed_mps: f64,
    /// True when [`WayPlan::speed_mps`] came from the [`HighwayPreset`] class default
    /// rather than from the way's own `maxspeed` tag, and the way is a motor way.
    ///
    /// A class default speed is jurisdiction-specific (V4/W1), so which lanes took one is
    /// exactly what a calibration pass and a reader of the report need to know.
    pub speed_from_preset: bool,
    /// Width of one lane, metres.
    pub lane_width_m: f64,
    /// Where [`WayPlan::lane_width_m`] came from.
    width_source: WidthSource,
    /// Which classes may use its lanes.
    pub allowed: ClassMask,
    /// The lane kind its lanes carry.
    pub kind: LaneKind,
    /// `turn:lanes` for the forward direction, **rightmost lane first**, or `None` when
    /// the tag is absent or unusable.
    pub turn_fwd: Option<Vec<Vec<TurnDirection>>>,
    /// `turn:lanes` for the backward direction, rightmost lane first.
    pub turn_bwd: Option<Vec<Vec<TurnDirection>>>,
    /// True for `junction=roundabout` or `junction=circular`.
    pub roundabout: bool,
    /// The street name, interned in the world's symbol table.
    pub name: Option<SymbolId>,
    /// `sidewalk=left|both` or `sidewalk:left=yes`.
    pub sidewalk_left: bool,
    /// `sidewalk=right|both` or `sidewalk:right=yes`.
    pub sidewalk_right: bool,
    /// The `layer` tag, which separates a bridge from what runs under it. Recorded but not
    /// yet used for geometry: Phase 1 is flat, so two ways on different layers that share
    /// no node simply do not connect. It does stop two segments on different layers being
    /// merged into one.
    pub layer: i32,
    /// `bridge=*` other than `no`. Recorded, and part of the merge key.
    pub bridge: bool,
    /// `tunnel=*` other than `no`. Recorded, and part of the merge key.
    pub tunnel: bool,
    /// How many levels below ground the way runs: `-layer` for a tunnel (at least 1 for
    /// `tunnel=yes`), `0` at grade. A `tunnel=building_passage` runs *through* a building
    /// at street level and stays at grade.
    pub levels_below: u8,
    /// How many levels above ground a bridge runs: its `layer` when positive, `0`
    /// otherwise (a bridge tagged at layer 0 crosses water or a gap, not a road).
    pub levels_above: u8,
}

impl WayPlan {
    /// Total lanes across both directions.
    fn total_lanes(&self) -> u8 {
        self.lanes_fwd.saturating_add(self.lanes_bwd)
    }

    /// Half the carriageway width, metres — the junction trimming radius this way
    /// contributes.
    fn half_width_m(&self) -> f64 {
        f64::from(self.total_lanes()) * self.lane_width_m * 0.5
    }
}

/// Reads one way's tags and decides what it becomes, or `None` if it becomes nothing.
///
/// Anomalies are counted, never raised: a way with an unreadable `maxspeed` still becomes
/// a road, at the class default speed.
pub fn classify_way(
    way: &RawWay,
    index: usize,
    options: &OsmOptions,
    symbols: &mut SymbolTable,
    report: &mut ImportReport,
) -> Option<WayPlan> {
    let highway = way.tags.get("highway")?;
    // No preset, no class defaults, so no way: `OsmOptions::validate` has already
    // refused this import, and there is no value to guess (V4/W1).
    let preset = options.highway_preset?;
    let Some(defaults) = preset.row(highway) else {
        report.note(Anomaly::UnknownHighwayValue, way.id);
        return None;
    };
    let wanted = match defaults.family {
        WayFamily::Motor => options.layers.drivable,
        WayFamily::Cycle => options.layers.cycle,
        WayFamily::Foot => options.layers.pedestrian,
    };
    if !wanted {
        return None;
    }

    // --- access ---------------------------------------------------------------
    let tag_says = |key: &str| {
        way.tags
            .get(key)
            .map(|v| (access_permits(v), access_denies(v)))
    };
    let mut allowed = match defaults.family {
        WayFamily::Motor => {
            if defaults.key == "busway" {
                ClassMask::BUS.union(ClassMask::EMERGENCY)
            } else {
                ClassMask::MOTOR_TRAFFIC
            }
        }
        WayFamily::Cycle => ClassMask::BICYCLE,
        WayFamily::Foot => ClassMask::PEDESTRIAN,
    };
    // General `access` first, then the more specific key, so the specific one wins.
    let specific = match defaults.family {
        WayFamily::Motor => "motor_vehicle",
        WayFamily::Cycle => "bicycle",
        WayFamily::Foot => "foot",
    };
    for key in ["access", "vehicle", specific] {
        if let Some((permits, denies)) = tag_says(key) {
            if denies {
                allowed = ClassMask::NONE;
            } else if permits {
                allowed = match defaults.family {
                    WayFamily::Motor => ClassMask::MOTOR_TRAFFIC,
                    WayFamily::Cycle => ClassMask::BICYCLE,
                    WayFamily::Foot => ClassMask::PEDESTRIAN,
                };
            }
        }
    }
    // An emergency vehicle may use a road that is private to everyone else; a way that is
    // physically absent (`access=no` on a construction site) is dropped either way,
    // because the mask is then empty for every class we model.
    if allowed.is_empty() {
        report.note(Anomaly::AccessDenied, way.id);
        return None;
    }
    // Bicycles share the carriageway where the law and the tags allow it.
    if defaults.family == WayFamily::Motor
        && options.bicycles_on_roads
        && !matches!(defaults.class, RoadClass::Motorway | RoadClass::Trunk)
        && !way.tags.get("bicycle").is_some_and(access_denies)
    {
        allowed = allowed.union(ClassMask::BICYCLE);
    }
    if defaults.family == WayFamily::Cycle && way.tags.get("foot").is_some_and(access_permits) {
        allowed = allowed.union(ClassMask::PEDESTRIAN);
    }
    // Rule 3 of the task and of 04-models.md §1.2: a pedestrian or cycle way is never
    // routable by a vehicle, whatever its tags say.
    if defaults.family != WayFamily::Motor {
        allowed = allowed.difference(ClassMask::MOTOR_TRAFFIC);
        if allowed.is_empty() {
            report.note(Anomaly::AccessDenied, way.id);
            return None;
        }
    }

    // --- direction ------------------------------------------------------------
    let roundabout = way
        .tags
        .get("junction")
        .is_some_and(|v| v == "roundabout" || v == "circular");
    let mut oneway = match way.tags.get("oneway") {
        Some(v) => match parse_oneway(v) {
            Oneway::Unparsable => {
                report.note(Anomaly::UnparsableOneway, way.id);
                Oneway::Absent
            }
            Oneway::Reversible => {
                report.note(Anomaly::ReversibleOneway, way.id);
                Oneway::Forward
            }
            other => other,
        },
        None => Oneway::Absent,
    };
    if oneway == Oneway::Absent {
        oneway = if roundabout || defaults.implicit_oneway {
            Oneway::Forward
        } else {
            Oneway::Both
        };
    }

    // --- lanes ----------------------------------------------------------------
    let count = |key: &str, report: &mut ImportReport| -> Option<u32> {
        let raw = way.tags.get(key)?;
        match parse_count(raw) {
            Some(v) => Some(v),
            None => {
                report.note(Anomaly::UnparsableLanes, way.id);
                None
            }
        }
    };
    let total = count("lanes", report);
    let forward_tag = count("lanes:forward", report);
    let backward_tag = count("lanes:backward", report);
    let default_lanes = u32::from(defaults.lanes);
    let (mut fwd, mut bwd) = match oneway {
        Oneway::Forward => (forward_tag.or(total).unwrap_or(default_lanes), 0),
        Oneway::Backward => (0, backward_tag.or(total).unwrap_or(default_lanes)),
        _ => {
            if forward_tag.is_some() || backward_tag.is_some() {
                (
                    forward_tag.unwrap_or_else(|| {
                        total
                            .map(|t| t.saturating_sub(backward_tag.unwrap_or(0)))
                            .unwrap_or(default_lanes)
                    }),
                    backward_tag.unwrap_or_else(|| {
                        total
                            .map(|t| t.saturating_sub(forward_tag.unwrap_or(0)))
                            .unwrap_or(default_lanes)
                    }),
                )
            } else if let Some(t) = total {
                if t % 2 == 1 && t > 1 {
                    // The extra lane goes to the forward direction. A centre turn lane is
                    // the usual cause, and the model has nowhere else to put it.
                    report.note(Anomaly::OddLaneSplit, way.id);
                }
                (t.div_ceil(2), t / 2)
            } else {
                (default_lanes, default_lanes)
            }
        }
    };
    // A two-way street needs at least one lane each way, whatever `lanes=1` claims, or it
    // would silently become one-way.
    if matches!(oneway, Oneway::Both | Oneway::Absent) {
        fwd = fwd.max(1);
        bwd = bwd.max(1);
    }
    let fwd = fwd.clamp(0, MAX_LANES_PER_DIRECTION) as u8;
    let bwd = bwd.clamp(0, MAX_LANES_PER_DIRECTION) as u8;
    if fwd == 0 && bwd == 0 {
        report.note(Anomaly::UnparsableLanes, way.id);
        return None;
    }

    // --- speed ----------------------------------------------------------------
    // `maxspeed` is a **vehicle** speed limit. Applying it to a footway, a path or a
    // staircase turns a street sign into a walking pace: a `highway=steps` with
    // `maxspeed=60` gave a staircase 16.7 m/s (R2). The tag is therefore read for the
    // motor family only, and a tag that was ignored is counted so the override is
    // visible in the report rather than silently dropped.
    let tagged_speed = way.tags.get("maxspeed").map(parse_maxspeed);
    let (speed_mps, speed_from_preset) = if defaults.family == WayFamily::Motor {
        match tagged_speed {
            Some(Maxspeed::Mps(v)) => (v, false),
            Some(Maxspeed::Unlimited) => {
                report.note(Anomaly::MaxspeedNone, way.id);
                (defaults.speed_mps, true)
            }
            Some(Maxspeed::Unparsable) => {
                report.note(Anomaly::UnparsableMaxspeed, way.id);
                (defaults.speed_mps, true)
            }
            _ => (defaults.speed_mps, true),
        }
    } else {
        match tagged_speed {
            Some(Maxspeed::Mps(_) | Maxspeed::Unlimited) => {
                report.note(Anomaly::SpeedTagIgnored, way.id);
            }
            Some(Maxspeed::Unparsable) => report.note(Anomaly::UnparsableMaxspeed, way.id),
            _ => {}
        }
        (defaults.speed_mps, false)
    };

    // --- turn lanes -----------------------------------------------------------
    // OSM writes `turn:lanes` leftmost lane first; the model indexes lane 0 as the
    // rightmost in the direction of travel (vwp-v1 §4.3), so the list is reversed.
    let turns = |key: &str, lanes: u8, report: &mut ImportReport| {
        let raw = way.tags.get(key)?;
        match parse_turn_lanes(raw) {
            Some(mut sets) if sets.len() == usize::from(lanes) => {
                sets.reverse();
                Some(sets)
            }
            _ => {
                report.note(Anomaly::UnparsableTurnLanes, way.id);
                None
            }
        }
    };
    let (turn_fwd, turn_bwd) = if oneway == Oneway::Forward {
        (
            turns("turn:lanes:forward", fwd, report).or_else(|| turns("turn:lanes", fwd, report)),
            None,
        )
    } else if oneway == Oneway::Backward {
        (
            None,
            turns("turn:lanes:backward", bwd, report).or_else(|| turns("turn:lanes", bwd, report)),
        )
    } else {
        (
            turns("turn:lanes:forward", fwd, report).or_else(|| turns("turn:lanes", fwd, report)),
            turns("turn:lanes:backward", bwd, report),
        )
    };

    // --- the rest -------------------------------------------------------------
    let sidewalk = way.tags.get("sidewalk").unwrap_or("");
    let sidewalk_left = matches!(sidewalk, "left" | "both")
        || way.tags.get("sidewalk:left").is_some_and(access_permits);
    let sidewalk_right = matches!(sidewalk, "right" | "both")
        || way.tags.get("sidewalk:right").is_some_and(access_permits);
    let (lane_width_m, width_source) = match defaults.family {
        WayFamily::Motor => motor_lane_width(
            &way.tags,
            defaults.class,
            fwd.saturating_add(bwd),
            way.id,
            options,
            report,
        ),
        WayFamily::Cycle => (options.cycleway_width_m, WidthSource::Option),
        WayFamily::Foot => (options.sidewalk_width_m, WidthSource::Option),
    };

    Some(WayPlan {
        way: index,
        osm_id: way.id,
        family: defaults.family,
        class: defaults.class,
        lanes_fwd: fwd,
        lanes_bwd: bwd,
        speed_mps,
        speed_from_preset,
        lane_width_m,
        width_source,
        allowed,
        kind: defaults.family.lane_kind(),
        turn_fwd,
        turn_bwd,
        roundabout,
        name: way
            .tags
            .get("name")
            .and_then(|n| symbols.intern_optional(n)),
        sidewalk_left,
        sidewalk_right,
        layer: way
            .tags
            .get("layer")
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(0),
        bridge: way.tags.get("bridge").is_some_and(|v| v != "no"),
        tunnel: way.tags.get("tunnel").is_some_and(|v| v != "no"),
        levels_below: {
            let tunnel = way.tags.get("tunnel").map(str::trim);
            let underground = tunnel.is_some_and(|v| v != "no" && v != "building_passage");
            let layer = way
                .tags
                .get("layer")
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(0);
            if underground {
                u8::try_from((-layer).max(1)).unwrap_or(u8::MAX)
            } else {
                0
            }
        },
        levels_above: {
            let bridge = way.tags.get("bridge").is_some_and(|v| v != "no");
            let layer = way
                .tags
                .get("layer")
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(0);
            if bridge && layer > 0 {
                u8::try_from(layer).unwrap_or(u8::MAX)
            } else {
                0
            }
        },
    })
}

/// The most lanes per direction this importer will believe: eight, which is wider than any
/// urban carriageway and stops a mistagged `lanes=900` from generating 900 lanes.
const MAX_LANES_PER_DIRECTION: u32 = 8;

// ---------------------------------------------------------------------------
// Stages 4 and 5: splitting at junction nodes, and collapsing trivial junctions
// ---------------------------------------------------------------------------

/// One piece of a way between two junction nodes.
///
/// Interior nodes are **shape**, not junctions: the legacy importer's habit of turning a
/// curve point into an intersection is exactly what 04-models.md §1.2's osm2streets-style
/// splitting rule exists to avoid.
#[derive(Debug, Clone, PartialEq)]
struct Segment {
    /// Index into the plan list; the plan supplies class, speed, width and access.
    plan: usize,
    /// `(way id, piece index)` of the lowest-numbered piece in this segment, which is what
    /// the edge id order sorts on. Taking the minimum makes the key independent of the
    /// order the collapse happened to merge pieces in.
    key: (i64, u32),
    /// The OSM way that reaches this segment's **first** node.
    ///
    /// A turn restriction names a way, and a junction that a way passes through twice
    /// gives two approach edges carrying that way id; matching on the whole `ways` list
    /// then bans a legal movement on the other approach (R4). The way at each end is
    /// what the restriction actually means.
    start_way: i64,
    /// The OSM way that reaches this segment's **last** node.
    end_way: i64,
    /// The node ids along it, first and last being junction nodes.
    nodes: Vec<i64>,
    /// Its geometry, world-local metres, parallel to `nodes`.
    points: Vec<Vec3>,
    /// Lanes running along `nodes`.
    fwd: u8,
    /// Lanes running against `nodes`.
    bwd: u8,
    /// `turn:lanes` for the forward direction at this segment's far end, rightmost first.
    turn_fwd: Option<Vec<Vec<TurnDirection>>>,
    /// `turn:lanes` for the backward direction at this segment's near end.
    turn_bwd: Option<Vec<Vec<TurnDirection>>>,
}

impl Segment {
    /// The node this segment starts at.
    fn start_node(&self) -> i64 {
        self.nodes[0]
    }

    /// The node it ends at.
    fn end_node(&self) -> i64 {
        self.nodes[self.nodes.len() - 1]
    }

    /// The same segment travelled the other way: node order, geometry, lane counts and
    /// turn assignments all reversed.
    fn reversed(&self) -> Segment {
        let mut nodes = self.nodes.clone();
        nodes.reverse();
        let mut points = self.points.clone();
        points.reverse();
        Segment {
            plan: self.plan,
            key: self.key,
            start_way: self.end_way,
            end_way: self.start_way,
            nodes,
            points,
            fwd: self.bwd,
            bwd: self.fwd,
            turn_fwd: self.turn_bwd.clone(),
            turn_bwd: self.turn_fwd.clone(),
        }
    }

    /// The same segment oriented to start at `node`, or `None` if it does not touch it.
    fn oriented_from(&self, node: i64) -> Option<Segment> {
        if self.start_node() == node {
            Some(self.clone())
        } else if self.end_node() == node {
            Some(self.reversed())
        } else {
            None
        }
    }

    /// Appends `next`, which must start where this one ends.
    fn extend(&mut self, next: &Segment) {
        self.nodes.extend_from_slice(&next.nodes[1..]);
        self.points.extend_from_slice(&next.points[1..]);
        // The chain now reaches the far end of the piece that was appended.
        self.end_way = next.end_way;
        // The downstream piece owns the turn assignment in each direction, because
        // `turn:lanes` describes the approach to the junction the piece *reaches*.
        self.turn_fwd = next.turn_fwd.clone();
        self.key = self.key.min(next.key);
    }
}

/// The attributes two segments must share before a junction between them can be collapsed.
///
/// Floats are compared by their bits, which is exact and needs no tolerance: the two
/// values came from the same tag parsing on the same way class, so they are either
/// identical or genuinely different.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MergeKey {
    family: WayFamily,
    class: RoadClass,
    speed_bits: u64,
    width_bits: u64,
    allowed: u16,
    kind: LaneKind,
    name: Option<SymbolId>,
    roundabout: bool,
    layer: i32,
    bridge: bool,
    tunnel: bool,
    sidewalk_left: bool,
    sidewalk_right: bool,
}

impl MergeKey {
    fn of(plan: &WayPlan) -> Self {
        Self {
            family: plan.family,
            class: plan.class,
            speed_bits: plan.speed_mps.to_bits(),
            width_bits: plan.lane_width_m.to_bits(),
            allowed: plan.allowed.bits(),
            kind: plan.kind,
            name: plan.name,
            roundabout: plan.roundabout,
            layer: plan.layer,
            bridge: plan.bridge,
            tunnel: plan.tunnel,
            sidewalk_left: plan.sidewalk_left,
            sidewalk_right: plan.sidewalk_right,
        }
    }
}

/// One run of a clipped way: its node ids and its geometry, parallel.
type ClipRun = (Vec<i64>, Vec<Vec3>);

/// Whether a point is on the inside of one of the clip box's four half-planes.
type HalfPlane = fn(&ClipBox, Vec3) -> bool;

/// Where the segment `a → b` crosses one of the clip box's four half-planes.
type HalfPlaneCrossing = fn(&ClipBox, Vec3, Vec3) -> Vec3;

/// What the bounding box does to one building or land-use ring: the ring to import and
/// whether the box cut it, or `None` when none of it is inside.
type RingFilter<'a> = dyn Fn(&[Vec3]) -> Option<(Vec<Vec3>, bool)> + 'a;

/// The bounding box, in world metres, that [`BboxClip::Clip`] cuts way geometry at.
///
/// The requested geodetic box grown by [`BboxClip::margin_m`], projected. The projection
/// is monotone in latitude and longitude, so a geodetic rectangle maps to an axis-aligned
/// one and the clip is four comparisons per point.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ClipBox {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl ClipBox {
    /// True if `p` is inside the box or on its boundary.
    fn contains(&self, p: Vec3) -> bool {
        p.x >= self.min_x && p.x <= self.max_x && p.y >= self.min_y && p.y <= self.max_y
    }

    /// Where the segment from `outside` to `inside` enters the box, or `None` if it never
    /// does.
    ///
    /// Liang–Barsky: multiplication, division and comparison only, so the crossing point
    /// is bit-identical on every platform. `inside` being inside makes `t1 = 1` the
    /// trivial exit, so what comes back is the entry parameter.
    fn entry(&self, outside: Vec3, inside: Vec3) -> Option<Vec3> {
        let dx = inside.x - outside.x;
        let dy = inside.y - outside.y;
        let mut t0 = 0.0f64;
        let mut t1 = 1.0f64;
        for (p, q) in [
            (-dx, outside.x - self.min_x),
            (dx, self.max_x - outside.x),
            (-dy, outside.y - self.min_y),
            (dy, self.max_y - outside.y),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
                continue;
            }
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                if r < t0 {
                    return None;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
        (t0 < t1).then(|| outside.lerp(inside, t0))
    }
}

/// One area ring cut to the clip box, or `None` when nothing of it is inside (V5).
///
/// Sutherland–Hodgman against the four half-planes of an axis-aligned rectangle:
/// multiplication, division and comparison only, so the result is bit-identical on every
/// platform. A building or a land-use zone that straddles the boundary keeps the part
/// inside the world and gains a wall on the boundary, which is the same trade a clipped
/// street makes; one wholly outside disappears.
///
/// Sub-millimetre and repeated vertices are dropped, because a ring is a polygon the
/// model will close and wind, and it refuses a ring of fewer than three distinct points.
fn clip_ring_to_box(ring: &[Vec3], clip: &ClipBox) -> Option<Vec<Vec3>> {
    if ring.len() < 3 {
        return None;
    }
    // Each edge as (inside test, intersection along a→b).
    let edges: [(HalfPlane, HalfPlaneCrossing); 4] = [
        (
            |c, p| p.x >= c.min_x,
            |c, a, b| {
                let t = (c.min_x - a.x) / (b.x - a.x);
                a.lerp(b, t)
            },
        ),
        (
            |c, p| p.x <= c.max_x,
            |c, a, b| {
                let t = (c.max_x - a.x) / (b.x - a.x);
                a.lerp(b, t)
            },
        ),
        (
            |c, p| p.y >= c.min_y,
            |c, a, b| {
                let t = (c.min_y - a.y) / (b.y - a.y);
                a.lerp(b, t)
            },
        ),
        (
            |c, p| p.y <= c.max_y,
            |c, a, b| {
                let t = (c.max_y - a.y) / (b.y - a.y);
                a.lerp(b, t)
            },
        ),
    ];
    let mut subject: Vec<Vec3> = ring.to_vec();
    // The ring arrives open or closed; the algorithm wants it open.
    if subject.len() >= 2 && subject[0] == subject[subject.len() - 1] {
        subject.pop();
    }
    for (inside, cross) in edges {
        if subject.len() < 3 {
            return None;
        }
        let mut out: Vec<Vec3> = Vec::with_capacity(subject.len() + 4);
        for i in 0..subject.len() {
            let a = subject[(i + subject.len() - 1) % subject.len()];
            let b = subject[i];
            let (a_in, b_in) = (inside(clip, a), inside(clip, b));
            if b_in {
                if !a_in {
                    let at = cross(clip, a, b);
                    if at.is_finite() {
                        out.push(at);
                    }
                }
                out.push(b);
            } else if a_in {
                let at = cross(clip, a, b);
                if at.is_finite() {
                    out.push(at);
                }
            }
        }
        subject = out;
    }
    let mut clipped = dedupe_points(subject);
    // `dedupe_points` only looks at successive points, so the wrap-around pair needs the
    // same treatment before the model is asked to close the ring.
    while clipped.len() >= 2
        && clipped[0].distance_2d(clipped[clipped.len() - 1]) < 2.0 * Q_POSITION_M
    {
        clipped.pop();
    }
    (clipped.len() >= 3).then_some(clipped)
}

/// One way's cleaned node list cut at the clip box, as the runs that remain inside (V5).
///
/// Returns one entry per run — its node ids and its geometry, parallel — and whether any
/// cut was made. A way wholly inside the box comes back unchanged in one run, which is
/// what keeps the ordinary case free of synthetic identity.
///
/// A run's cut end carries a **synthetic node id** taken from `next_id`, counting down:
/// the crossing point is not an OSM node, but the junction it becomes still needs an
/// identity, and everything downstream keys junctions by node id. The ids are negative
/// and start below every id the extract carries, so they cannot collide with a real one,
/// and they are handed out in way order so they are the same on every run of the
/// importer.
///
/// A crossing point closer than a millimetre to the inside vertex it would precede is
/// dropped instead: the way already ends on the boundary, and a sub-millimetre segment is
/// what [`Lane::new`] refuses.
fn clip_runs(
    nodes: &[i64],
    geometry: &[Vec3],
    clip: &ClipBox,
    next_id: &mut i64,
) -> (Vec<ClipRun>, bool) {
    debug_assert_eq!(nodes.len(), geometry.len());
    let inside: Vec<bool> = geometry.iter().map(|p| clip.contains(*p)).collect();
    if inside.iter().all(|k| *k) {
        return (vec![(nodes.to_vec(), geometry.to_vec())], false);
    }
    let mut runs: Vec<ClipRun> = Vec::new();
    let mut open: Option<ClipRun> = None;
    let far_enough = |a: Vec3, b: Vec3| a.distance_2d(b) >= 2.0 * Q_POSITION_M;
    for i in 0..nodes.len() {
        if inside[i] {
            if open.is_none() {
                let mut run = (Vec::new(), Vec::new());
                if i > 0 {
                    if let Some(at) = clip.entry(geometry[i - 1], geometry[i]) {
                        if far_enough(at, geometry[i]) {
                            *next_id -= 1;
                            run.0.push(*next_id);
                            run.1.push(at);
                        }
                    }
                }
                open = Some(run);
            }
            let run = open.as_mut().expect("the run was just opened");
            run.0.push(nodes[i]);
            run.1.push(geometry[i]);
        } else if let Some(mut run) = open.take() {
            if let Some(at) = clip.entry(geometry[i], geometry[i - 1]) {
                let last = run.1[run.1.len() - 1];
                if far_enough(at, last) {
                    *next_id -= 1;
                    run.0.push(*next_id);
                    run.1.push(at);
                }
            }
            runs.push(run);
        }
    }
    if let Some(run) = open.take() {
        runs.push(run);
    }
    runs.retain(|(n, _)| n.len() >= 2);
    (runs, true)
}

/// Cuts every planned way into segments at the junction nodes of its own family.
///
/// # The splitting rule
///
/// A node is a junction of the motor network when **two or more motor ways** pass through
/// it, and of the soft (foot and cycle) network when two or more soft ways do. Every way's
/// first and last node is a junction of its own family, and a closed way is additionally
/// cut at its middle node so that it does not become a single edge that leaves and
/// re-enters one junction.
///
/// Counting per family is what keeps a crosswalk from cutting the street it crosses into
/// two edges: the shared node has one motor way and one foot way, so it is a junction of
/// neither network on its own. A soft way *is* cut where it meets a motor junction, so
/// that a pavement ends where the road it serves does.
///
/// # The bounding-box clip
///
/// When `clip` is given, each way's geometry is first cut at the box ([`clip_runs`]) and
/// every run is then split at its own junction nodes. Junction *counting* still runs over
/// the whole extract, so a node shared by two ways is a junction even when one of them is
/// mostly outside the box.
fn split_ways(
    plans: &[WayPlan],
    file: &OsmFile,
    points: &[Vec3],
    clip: Option<&ClipBox>,
    next_synthetic: &mut i64,
    report: &mut ImportReport,
) -> Vec<Segment> {
    // Pass 1: how many ways of each family use each node.
    let mut motor_uses: BTreeMap<i64, u32> = BTreeMap::new();
    let mut soft_uses: BTreeMap<i64, u32> = BTreeMap::new();
    for plan in plans {
        let way = &file.ways[plan.way];
        let counter = if plan.family == WayFamily::Motor {
            &mut motor_uses
        } else {
            &mut soft_uses
        };
        for node in &way.nodes {
            *counter.entry(*node).or_insert(0) += 1;
        }
    }

    let mut motor_cuts: BTreeSet<i64> = motor_uses
        .iter()
        .filter(|(_, n)| **n >= 2)
        .map(|(id, _)| *id)
        .collect();
    let mut soft_cuts: BTreeSet<i64> = soft_uses
        .iter()
        .filter(|(_, n)| **n >= 2)
        .map(|(id, _)| *id)
        .collect();
    // A soft way is also cut where it meets the motor network.
    for node in soft_uses.keys() {
        if motor_uses.contains_key(node) && motor_cuts.contains(node) {
            soft_cuts.insert(*node);
        }
    }
    for plan in plans {
        let way = &file.ways[plan.way];
        let cuts = if plan.family == WayFamily::Motor {
            &mut motor_cuts
        } else {
            &mut soft_cuts
        };
        if let (Some(first), Some(last)) = (way.nodes.first(), way.nodes.last()) {
            cuts.insert(*first);
            cuts.insert(*last);
            if first == last && way.nodes.len() >= 4 {
                cuts.insert(way.nodes[way.nodes.len() / 2]);
            }
        }
    }

    // Pass 2: clean each way's node list and cut it.
    let mut segments = Vec::new();
    for (plan_index, plan) in plans.iter().enumerate() {
        let way = &file.ways[plan.way];
        let cuts = if plan.family == WayFamily::Motor {
            &motor_cuts
        } else {
            &soft_cuts
        };
        let mut nodes: Vec<i64> = Vec::with_capacity(way.nodes.len());
        let mut geometry: Vec<Vec3> = Vec::with_capacity(way.nodes.len());
        let mut missing = false;
        let mut duplicate = false;
        for node in &way.nodes {
            let Some(index) = file.node_index(*node) else {
                missing = true;
                continue;
            };
            let point = points[index];
            if let Some(previous) = geometry.last() {
                // Two nodes at the same millimetre make a zero-length segment, which the
                // lane model rejects; the second is shape noise and is dropped.
                if previous.distance_2d(point) < 2.0 * Q_POSITION_M && nodes.last() != Some(node) {
                    duplicate = true;
                    continue;
                }
                if nodes.last() == Some(node) {
                    duplicate = true;
                    continue;
                }
            }
            nodes.push(*node);
            geometry.push(point);
        }
        if missing {
            report.note(Anomaly::MissingNode, way.id);
        }
        if duplicate {
            report.note(Anomaly::DuplicateNode, way.id);
        }
        if nodes.len() < 2 {
            report.note(Anomaly::WayTooShort, way.id);
            continue;
        }
        // The way's own ends, before any clip: `turn:lanes` describes the approach to the
        // junction the way *reaches*, so a run that stops at the box boundary instead must
        // not carry it.
        let way_start = nodes[0];
        let way_end = nodes[nodes.len() - 1];

        let runs = match clip {
            Some(clip) => {
                let (runs, cut) = clip_runs(&nodes, &geometry, clip, next_synthetic);
                if cut {
                    report.note(Anomaly::ClippedGeometry, way.id);
                    report.counts.ways_clipped += 1;
                    report.counts.clipped_runs += runs.len() as u64;
                    if runs.is_empty() {
                        report.note(Anomaly::ClippedOut, way.id);
                    }
                }
                runs
            }
            None => vec![(nodes, geometry)],
        };

        let mut piece = 0u32;
        for (nodes, geometry) in runs {
            if nodes.len() < 2 {
                continue;
            }
            let mut start = 0usize;
            for i in 1..nodes.len() {
                let last = i == nodes.len() - 1;
                if !last && !cuts.contains(&nodes[i]) {
                    continue;
                }
                segments.push(Segment {
                    plan: plan_index,
                    key: (plan.osm_id, piece),
                    start_way: plan.osm_id,
                    end_way: plan.osm_id,
                    nodes: nodes[start..=i].to_vec(),
                    points: geometry[start..=i].to_vec(),
                    fwd: plan.lanes_fwd,
                    bwd: plan.lanes_bwd,
                    // Only the piece that reaches the way's far end carries the way's
                    // `turn:lanes`: the tag describes the approach to one junction.
                    turn_fwd: if nodes[i] == way_end {
                        plan.turn_fwd.clone()
                    } else {
                        None
                    },
                    turn_bwd: if start == 0 && nodes[0] == way_start {
                        plan.turn_bwd.clone()
                    } else {
                        None
                    },
                });
                piece += 1;
                start = i;
            }
        }
    }
    segments
}

/// Merges the segments that meet at a node where exactly two compatible segments meet, and
/// deletes that junction — the one osm2streets simplification this importer implements.
///
/// OSM splits a single street into many ways, at every attribute change and at every
/// administrative boundary, so without this a straight avenue becomes a chain of junctions
/// with two approaches each, and every one of them would get a signal plan, a conflict
/// matrix and a set of internal connectors it does not need.
///
/// Two segments may merge only if they agree on family, class, speed, lane width, access
/// mask, name, roundabout flag and layer ([`MergeKey`]) *and* on lane counts once oriented
/// the same way. Merging never creates a self-loop: a node whose two segments have the
/// same far end is left alone.
///
/// A node named as the `via` of a turn restriction is never collapsed, whatever its shape:
/// the restriction has no movement to attach to if the junction it names stops existing.
///
/// The walk is deterministic: chains are started from the lowest node id upwards, and a
/// chain of segments that is a closed loop with no ordinary junction on it keeps its
/// lowest node id as a junction.
fn collapse_trivial_junctions(
    segments: Vec<Segment>,
    plans: &[WayPlan],
    protected: &BTreeSet<i64>,
    report: &mut ImportReport,
) -> (Vec<Segment>, u64) {
    let mut ends: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, seg) in segments.iter().enumerate() {
        ends.entry(seg.start_node()).or_default().push(i);
        ends.entry(seg.end_node()).or_default().push(i);
    }

    // `through[node]` is `Some((a, b))` when the node joins exactly two mergeable segments.
    let through = |node: i64| -> Option<(usize, usize)> {
        if protected.contains(&node) {
            return None;
        }
        let at = ends.get(&node)?;
        if at.len() != 2 || at[0] == at[1] {
            return None;
        }
        let (a, b) = (&segments[at[0]], &segments[at[1]]);
        if MergeKey::of(&plans[a.plan]) != MergeKey::of(&plans[b.plan]) {
            return None;
        }
        // Orient both so that `a` arrives at the node and `b` leaves it.
        let a_in = if a.end_node() == node {
            a.clone()
        } else {
            a.reversed()
        };
        let b_out = b.oriented_from(node)?;
        if a_in.fwd != b_out.fwd || a_in.bwd != b_out.bwd {
            return None;
        }
        if a_in.start_node() == b_out.end_node() {
            return None; // merging would close a loop onto one junction
        }
        Some((at[0], at[1]))
    };

    let mut consumed = vec![false; segments.len()];
    let mut out: Vec<Segment> = Vec::new();
    let mut collapsed = 0u64;

    // Walks a chain starting at `node` along `seed`, merging through every trivial node.
    let walk = |node: i64, seed: usize, consumed: &mut Vec<bool>, collapsed: &mut u64| {
        let mut chain = segments[seed].oriented_from(node)?;
        consumed[seed] = true;
        let mut current = seed;
        loop {
            let end = chain.end_node();
            let Some((a, b)) = through(end) else { break };
            let other = if a == current {
                b
            } else if b == current {
                a
            } else {
                break;
            };
            if consumed[other] {
                break;
            }
            let Some(next) = segments[other].oriented_from(end) else {
                break;
            };
            chain.extend(&next);
            consumed[other] = true;
            *collapsed += 1;
            current = other;
        }
        Some(chain)
    };

    for (node, at) in &ends {
        if through(*node).is_some() {
            continue;
        }
        for seed in at {
            if consumed[*seed] {
                continue;
            }
            if let Some(chain) = walk(*node, *seed, &mut consumed, &mut collapsed) {
                out.push(chain);
            }
        }
    }
    // Anything left is a ring of trivial nodes: keep its lowest node id as the junction.
    for seed in 0..segments.len() {
        if consumed[seed] {
            continue;
        }
        let node = segments[seed].start_node().min(segments[seed].end_node());
        if let Some(chain) = walk(node, seed, &mut consumed, &mut collapsed) {
            out.push(chain);
        }
    }

    // A segment that starts and ends at the same junction has no movement through it.
    let mut kept = Vec::with_capacity(out.len());
    for seg in out {
        if seg.start_node() == seg.end_node() {
            report.note(Anomaly::SelfLoopSegment, plans[seg.plan].osm_id);
            continue;
        }
        kept.push(seg);
    }
    kept.sort_by_key(|s| s.key);
    (kept, collapsed)
}

// ---------------------------------------------------------------------------
// Stage 6: polyline geometry
// ---------------------------------------------------------------------------

/// The shortest lane this importer will emit, metres. Below it a lane is noise.
const MIN_LANE_LENGTH_M: f64 = 1.0;

/// How far one `layer` level puts a roadway below ground (a tunnel) or above it (a
/// bridge over a road), metres.
///
/// **This importer's choice**: OSM's `layer` is an ordering, not a height. 6 m is the
/// 4.9 m (16 ft) minimum vertical clearance of a road under a structure (AASHTO Green
/// Book 2018 §8.2) plus the structure, rounded; a second level is twice that.
const TUNNEL_LEVEL_DEPTH_M: f64 = 6.0;

/// The smallest junction trimming radius, metres.
const MIN_JUNCTION_RADIUS_M: f64 = 1.0;

/// The largest junction trimming radius, metres — a guard against one mistagged
/// `lanes=8` boulevard eating a whole block.
const MAX_JUNCTION_RADIUS_M: f64 = 25.0;

/// The largest trimming radius applied to a pedestrian or cycle way, metres.
///
/// Soft ways are trimmed by their own family's width, not by the widest carriageway at the
/// junction. Trimming a 12 m crosswalk back by the 14 m half-width of the avenue it
/// crosses would delete it, and the pavement it serves with it: on the real Manhattan
/// extract this one rule is the difference between 823 dropped edges and 6.
const MAX_SOFT_JUNCTION_RADIUS_M: f64 = 5.0;

/// The shortest lane that is kept once it has been offset and trimmed, metres.
///
/// [`MIN_LANE_LENGTH_M`] is what the trimming *aims* to leave; offsetting a lane around a
/// corner can shorten it a little further, and a 0.4 m lane is still a usable edge of the
/// graph, whereas dropping it disconnects whatever was on the other side.
const MIN_KEPT_LANE_M: f64 = 0.2;

/// How many points a turning connector is sampled at.
const TURN_SAMPLES: usize = 7;

/// The horizontal length of a polyline, metres.
fn polyline_length(points: &[Vec3]) -> f64 {
    let mut total = 0.0;
    for pair in points.windows(2) {
        total += pair[0].distance_2d(pair[1]);
    }
    total
}

/// Drops points that are closer than one millimetre to their predecessor.
///
/// [`Lane::new`] rejects a centreline with a sub-millimetre segment, and offsetting a
/// polyline at a hairpin can produce one, so every generated polyline passes through here.
fn dedupe_points(points: Vec<Vec3>) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::with_capacity(points.len());
    for p in points {
        if let Some(previous) = out.last() {
            if previous.distance_2d(p) < 2.0 * Q_POSITION_M {
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// The part of `points` between arc lengths `from_s` and `to_s`, both measured from the
/// start.
///
/// Returns `None` when the requested range is empty or degenerate. The cut ends are
/// interpolated, so the result starts and ends exactly at the requested arc lengths.
fn trim_polyline(points: &[Vec3], from_s: f64, to_s: f64) -> Option<Vec<Vec3>> {
    if points.len() < 2 || to_s <= from_s || !to_s.is_finite() || !from_s.is_finite() {
        return None;
    }
    let mut out: Vec<Vec3> = Vec::with_capacity(points.len());
    let mut walked = 0.0;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let step = a.distance_2d(b);
        if step <= 0.0 {
            continue;
        }
        let (s0, s1) = (walked, walked + step);
        walked = s1;
        if s1 < from_s || s0 > to_s {
            continue;
        }
        let t0 = ((from_s - s0) / step).clamp(0.0, 1.0);
        let t1 = ((to_s - s0) / step).clamp(0.0, 1.0);
        if t1 <= t0 {
            continue;
        }
        if out.is_empty() {
            out.push(a.lerp(b, t0));
        }
        out.push(a.lerp(b, t1));
    }
    let out = dedupe_points(out);
    (out.len() >= 2).then_some(out)
}

/// `points` shifted `d_m` to the **left** of its direction of travel (negative is right),
/// with the inverted vertices and the loops they form pruned away.
///
/// Each vertex moves along the mitre of its two adjacent segment normals, so a lane keeps
/// a constant distance from the centreline around a corner rather than cutting it. Two
/// things then have to be repaired, and before this they were not (R1):
///
/// 1. **A bend tighter than the offset inverts.** Offsetting a corner whose radius of
///    curvature is smaller than `|d_m|` moves the inner vertex *past* its neighbours, so
///    that part of the lane runs backwards. The mitre is capped at `1 / MITRE_MIN_COS`
///    times the offset — beyond that the join collapses to the average normal at exactly
///    `|d_m|`, a bevel rather than a spike — and any vertex whose offset segment still
///    runs against the source segment it came from is dropped.
/// 2. **What is left may still cross itself.** A crossing makes arc length non-injective
///    in space: on the Phase 1 extract lane `l7790` had one point at both `s = 15.24 m`
///    and `s = 24.76 m`, 9.52 m apart, and `Lane::project_point` could only return one of
///    them, so every gap, leader and lateral offset a mobility model computed from
///    `(lane, s)` on that lane was wrong. Each crossing is cut out at the crossing point,
///    which keeps the route and removes the loop.
///
/// Returns the offset polyline and whether either repair fired, so the caller can count
/// it as [`Anomaly::SelfIntersectingLane`] rather than let it pass unseen. The first and
/// last vertices are never dropped: a lane has to start and end where its junction
/// expects it.
///
/// Arithmetic and `sqrt` only — no transcendental — so the result is bit-identical on
/// every platform.
fn offset_polyline(points: &[Vec3], d_m: f64) -> (Vec<Vec3>, bool) {
    /// The smallest cosine of the half-angle the mitre is allowed to divide by. Beyond
    /// it the join is bevelled instead, which costs accuracy only at corners sharper
    /// than about 145°, where the road itself is not a smooth curve anyway.
    const MITRE_MIN_COS: f64 = 0.35;
    if d_m == 0.0 || points.len() < 2 {
        return (points.to_vec(), false);
    }
    let normal = |a: Vec3, b: Vec3| -> Vec3 {
        let d = (b - a).normalized();
        Vec3::new(-d.y, d.x, 0.0)
    };
    let n = points.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let before = (i > 0).then(|| normal(points[i - 1], points[i]));
        let after = (i + 1 < n).then(|| normal(points[i], points[i + 1]));
        let shift = match (before, after) {
            (Some(a), Some(b)) => {
                let sum = a + b;
                let len = sum.norm_2d();
                if len < 1e-9 {
                    a.scale(d_m)
                } else {
                    let mitre = sum.scale(1.0 / len);
                    let cos_half = mitre.x * a.x + mitre.y * a.y;
                    if cos_half < MITRE_MIN_COS {
                        // Bevel: stay at the offset distance instead of running out along
                        // a mitre that would overshoot the bend it is turning.
                        mitre.scale(d_m)
                    } else {
                        mitre.scale(d_m / cos_half)
                    }
                }
            }
            (Some(a), None) | (None, Some(a)) => a.scale(d_m),
            (None, None) => Vec3::ZERO,
        };
        out.push(points[i] + shift);
    }
    let mut source = points.to_vec();
    let mut repaired = prune_inverted_vertices(&mut source, &mut out);
    repaired |= cut_self_crossings(&mut out);
    (out, repaired)
}

/// Drops the offset vertices whose mitre shift reversed the direction of travel.
///
/// `source` and `out` stay parallel — `source[i]` is the centreline vertex `out[i]` was
/// offset from — so the test is exact: an offset segment must point the same way as the
/// source segment it came from, and `dot < 0` says it does not. Of the two ends of an
/// inverted segment the one that moved furthest from its source is the one at fault, and
/// the first and last vertices are never candidates.
///
/// Returns true if anything was dropped.
fn prune_inverted_vertices(source: &mut Vec<Vec3>, out: &mut Vec<Vec3>) -> bool {
    debug_assert_eq!(source.len(), out.len());
    let mut dropped = false;
    loop {
        let n = out.len();
        if n < 3 {
            break;
        }
        let mut victim: Option<usize> = None;
        for i in 0..n - 1 {
            let forward = out[i + 1] - out[i];
            let along = source[i + 1] - source[i];
            if forward.x * along.x + forward.y * along.y >= 0.0 {
                continue;
            }
            victim = Some(if i == 0 {
                i + 1
            } else if i + 2 == n {
                i
            } else {
                let moved_a = (out[i] - source[i]).norm_2d();
                let moved_b = (out[i + 1] - source[i + 1]).norm_2d();
                if moved_a >= moved_b { i } else { i + 1 }
            });
            break;
        }
        match victim {
            Some(k) if k > 0 && k + 1 < n => {
                out.remove(k);
                source.remove(k);
                dropped = true;
            }
            _ => break,
        }
    }
    dropped
}

/// Cuts every loop out of a polyline that crosses itself, at the crossing point.
///
/// `... p[i] → p[i+1] … p[j] → p[j+1] ...` where the two segments cross becomes
/// `... p[i] → crossing → p[j+1] ...`: the same route, without the loop, and with the
/// endpoints untouched. Returns true if anything was cut.
fn cut_self_crossings(points: &mut Vec<Vec3>) -> bool {
    /// A polyline of `n` vertices has fewer than `n` loops to cut, and a lane centreline
    /// is short; the bound is here so a pathological input cannot spin.
    const MAX_CUTS: usize = 64;
    let mut repaired = false;
    for _ in 0..MAX_CUTS {
        let Some((i, j, at)) = first_self_crossing(points) else {
            break;
        };
        points.splice(i + 1..=j, [at]);
        *points = dedupe_points(core::mem::take(points));
        repaired = true;
        if points.len() < 3 {
            break;
        }
    }
    repaired
}

/// The first pair of non-adjacent segments of `points` that properly cross, with the
/// crossing point: `(first segment's start index, second segment's start index, point)`.
fn first_self_crossing(points: &[Vec3]) -> Option<(usize, usize, Vec3)> {
    if points.len() < 4 {
        return None;
    }
    for i in 0..points.len() - 1 {
        for j in i + 2..points.len() - 1 {
            if let Some(at) = segment_crossing(points[i], points[i + 1], points[j], points[j + 1]) {
                return Some((i, j, at));
            }
        }
    }
    None
}

/// Where two segments properly cross, or `None`.
///
/// "Properly" excludes touching at an endpoint and excludes the parallel case, so two
/// consecutive segments of a polyline never count as a crossing. Cross products and one
/// division, so the answer is the same on every platform.
fn segment_crossing(p: Vec3, p2: Vec3, q: Vec3, q2: Vec3) -> Option<Vec3> {
    let r = p2 - p;
    let s = q2 - q;
    let denominator = r.x * s.y - r.y * s.x;
    if denominator == 0.0 || !denominator.is_finite() {
        return None;
    }
    let qp = q - p;
    let t = (qp.x * s.y - qp.y * s.x) / denominator;
    let u = (qp.x * r.y - qp.y * r.x) / denominator;
    if !(t > 0.0 && t < 1.0 && u > 0.0 && u < 1.0) {
        return None;
    }
    Some(Vec3::new(
        p.x + t * r.x,
        p.y + t * r.y,
        p.z + t * (p2.z - p.z),
    ))
}

/// A junction connector from `(start, heading_in)` to `(end, heading_out)`.
///
/// A movement whose two tangents are nearly collinear and whose endpoints lie on that line
/// becomes a straight two-point lane; anything else becomes a quadratic Bézier through the
/// point where the two tangent lines meet, sampled at [`TURN_SAMPLES`] points. Polynomial
/// arithmetic only, so a turn's geometry is bit-identical on every platform.
fn connector_geometry(start: Vec3, heading_in: f64, end: Vec3, heading_out: f64) -> Vec<Vec3> {
    let turn = normalise_angle(heading_out - heading_in);
    let (sin_in, cos_in) = math::sin_cos(heading_in);
    let (sin_out, cos_out) = math::sin_cos(heading_out);
    let chord = end - start;
    // How far the end lies off the approach's own line: a straight connector between two
    // parallel but laterally offset lanes would meet both at an angle, so it takes the
    // S-shaped cubic instead.
    let offset = (chord.x * sin_in - chord.y * cos_in).abs();
    if turn.abs() < 1e-3 {
        if offset > 0.05 {
            return cubic_connector(start, heading_in, end, heading_out);
        }
        return vec![start, end];
    }
    // Solve `start + u · dir_in = end - v · dir_out` for u.
    let denominator = cos_in * sin_out - sin_in * cos_out;
    if denominator.abs() < 1e-9 {
        if offset > 0.05 {
            return cubic_connector(start, heading_in, end, heading_out);
        }
        return vec![start, end];
    }
    let u = (chord.x * sin_out - chord.y * cos_out) / denominator;
    let control = Vec3::new(start.x + cos_in * u, start.y + sin_in * u, start.z);
    // The tangent lines must meet *between* the two ends: ahead of the start along the
    // approach heading and behind the end along the departure heading, and not far out of
    // the junction. A meeting point beyond the end — two nearly parallel lanes offset
    // sideways, the commonest case on an avenue where a lane is dropped — made the
    // quadratic run past the end and double back on itself: a 40 m connector across a
    // 13 m junction with a 180° heading reversal in it, which the traffic auditor found as
    // vehicles reversing inside junctions and overlapping on one lane. Those take the
    // tangent-continuous cubic instead.
    let v = (end.x - control.x) * cos_out + (end.y - control.y) * sin_out;
    let reach = 2.0 * chord.norm_2d() + 1.0;
    if !control.is_finite() || u <= 0.0 || v <= 0.0 || u > reach || v > reach {
        return cubic_connector(start, heading_in, end, heading_out);
    }
    (0..TURN_SAMPLES)
        .map(|i| {
            let t = i as f64 / (TURN_SAMPLES - 1) as f64;
            let w = 1.0 - t;
            Vec3::new(
                w * w * start.x + 2.0 * w * t * control.x + t * t * end.x,
                w * w * start.y + 2.0 * w * t * control.y + t * t * end.y,
                w * w * start.z + 2.0 * w * t * control.z + t * t * end.z,
            )
        })
        .collect()
}

/// A tangent-continuous cubic Bézier from `(start, heading_in)` to `(end, heading_out)`,
/// with both handles a third of the chord long — the Hermite curve with the chord's own
/// speed, which leaves along the approach heading, arrives along the departure heading
/// and never overshoots either end. Used where the quadratic's tangent intersection lies
/// outside the junction; a pair of exactly parallel, exactly collinear ends stays a
/// straight two-point lane.
fn cubic_connector(start: Vec3, heading_in: f64, end: Vec3, heading_out: f64) -> Vec<Vec3> {
    let (sin_in, cos_in) = math::sin_cos(heading_in);
    let (sin_out, cos_out) = math::sin_cos(heading_out);
    let k = start.distance_2d(end) / 3.0;
    let p1 = Vec3::new(start.x + cos_in * k, start.y + sin_in * k, start.z);
    let p2 = Vec3::new(end.x - cos_out * k, end.y - sin_out * k, end.z);
    (0..TURN_SAMPLES)
        .map(|i| {
            let t = i as f64 / (TURN_SAMPLES - 1) as f64;
            let w = 1.0 - t;
            let (b0, b1, b2, b3) = (w * w * w, 3.0 * w * w * t, 3.0 * w * t * t, t * t * t);
            Vec3::new(
                b0 * start.x + b1 * p1.x + b2 * p2.x + b3 * end.x,
                b0 * start.y + b1 * p1.y + b2 * p2.y + b3 * end.y,
                b0 * start.z + b1 * p1.z + b2 * p2.z + b3 * end.z,
            )
        })
        .collect()
}

/// The counter-clockwise convex hull of `points`, closed (the last point repeats the
/// first), or an empty vector when there are fewer than three distinct points.
///
/// Andrew's monotone chain: a sort and two linear passes, with `f64::total_cmp` as the
/// order so that the result does not depend on how the platform compares equal-but-signed
/// zeroes. This is the junction area of 04-models.md §1.2 — the polygon spanned by the
/// ends of the lanes that meet there.
fn convex_hull(points: &[Vec3]) -> Vec<Vec3> {
    let mut sorted: Vec<Vec3> = points.to_vec();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    sorted.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if sorted.len() < 3 {
        return Vec::new();
    }
    let cross = |o: Vec3, a: Vec3, b: Vec3| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Vec3> = Vec::with_capacity(sorted.len() * 2);
    for &p in &sorted {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    let lower = hull.len() + 1;
    for &p in sorted.iter().rev() {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    if hull.len() < 4 {
        return Vec::new();
    }
    hull
}

// ---------------------------------------------------------------------------
// Stage 7: the lane-level network
// ---------------------------------------------------------------------------

/// One edge of the network while it is being built, with the OSM bookkeeping the later
/// stages need and the finished [`Edge`] does not carry.
#[derive(Debug, Clone)]
struct EdgeInfo {
    /// Index into the plan list.
    plan: usize,
    /// Index into the segment list, so that the opposite carriageway of one segment can be
    /// recognised as a U-turn rather than treated as a separate street.
    segment: usize,
    /// Its family.
    family: WayFamily,
    /// Its lanes, rightmost first.
    lanes: Vec<LaneId>,
    /// The `turn:lanes` assignment for the approach this edge makes, rightmost lane first.
    turns: Option<Vec<Vec<TurnDirection>>>,
    /// The OSM way that reaches this edge's `from` junction — what a restriction's `to`
    /// member must name for this edge to be the departure (R4).
    near_way: i64,
    /// The OSM way that reaches this edge's `to` junction — what a restriction's `from`
    /// member must name for this edge to be the approach (R4).
    far_way: i64,
}

/// One movement through a junction, while the junction is being built.
#[derive(Debug, Clone, Copy)]
struct Movement {
    /// The approach lane.
    from_lane: LaneId,
    /// The departure lane.
    to_lane: LaneId,
    /// Its connector lane.
    internal: LaneId,
    /// Which way it turns.
    turn: TurnDirection,
    /// The heading of travel *into* the junction, radians ENU — the input to the
    /// priority-to-the-right rule and to the signal phase grouping.
    approach_heading: f64,
    /// Index into the edge-info list of the approach.
    from_edge: usize,
    /// Index into the edge-info list of the departure.
    to_edge: usize,
    /// False once a turn restriction has banned it.
    permitted: bool,
}

/// A movement whose approach and departure lane ends are the same millimetre, so it has
/// no connector lane.
///
/// It happens where a segment was too short to trim back from both of its junctions. Such
/// a movement used to be emitted straight into the connection list and never entered
/// `Net::movements`, so [`apply_restrictions`] and [`conflict_matrix`] never saw it and a
/// banned turn stayed permitted (R3). It is now carried here, and
/// [`apply_restrictions`] walks it. It still has no conflict-matrix row — that matrix is
/// indexed by connector lane, and [`World::validate`] requires one row per connector —
/// which [`Anomaly::ZeroLengthConnector`] records.
#[derive(Debug, Clone, Copy)]
struct DirectMovement {
    /// The approach lane.
    from_lane: LaneId,
    /// The departure lane.
    to_lane: LaneId,
    /// Which way it turns.
    turn: TurnDirection,
    /// The junction it crosses, as an index into `Net::movements`.
    junction: usize,
    /// Index into the edge-info list of the approach.
    from_edge: usize,
    /// Index into the edge-info list of the departure.
    to_edge: usize,
    /// False once a turn restriction has banned it.
    permitted: bool,
}

/// Everything the network stages share.
struct Net {
    lanes: Vec<Lane>,
    edges: Vec<Edge>,
    junctions: Vec<Junction>,
    edge_info: Vec<EdgeInfo>,
    /// Edge-info indices arriving at each junction.
    incoming: Vec<Vec<usize>>,
    /// Edge-info indices leaving each junction.
    outgoing: Vec<Vec<usize>>,
    /// Movements through each junction, in movement order.
    movements: Vec<Vec<Movement>>,
    /// The OSM node id behind each junction.
    junction_nodes: Vec<i64>,
    /// Junction id by OSM node id.
    junction_of: BTreeMap<i64, JunctionId>,
}

/// True if `tagged` (from `turn:lanes`) should be taken to permit `actual` (the geometry).
///
/// OSM's turn vocabulary is coarser than the geometry: a mapper writes `left` for anything
/// that leaves to the left, and the junction may be skewed enough that the model calls it
/// a slight left. Matching exactly would ban legal movements, which is worse than
/// permitting a near miss.
fn turn_matches(tagged: TurnDirection, actual: TurnDirection) -> bool {
    use TurnDirection::{Left, Right, SlightLeft, SlightRight, Straight, UTurn};
    match tagged {
        Straight => matches!(actual, Straight | SlightLeft | SlightRight),
        Left | SlightLeft => matches!(actual, Left | SlightLeft),
        Right | SlightRight => matches!(actual, Right | SlightRight),
        UTurn => actual == UTurn,
    }
}

/// Which approach lanes may make a movement, by the geometric rule, when `turn:lanes` says
/// nothing.
///
/// Right turns leave from the rightmost lane, left turns and U-turns from the leftmost,
/// and everything else from any lane — the same rule the procedural generator uses and the
/// one every highway code assumes in the absence of markings. A single-lane approach makes
/// every movement from its one lane.
fn lanes_for_turn(turn: TurnDirection, lanes: usize) -> Vec<usize> {
    if lanes <= 1 {
        return vec![0];
    }
    match turn {
        TurnDirection::Right | TurnDirection::SlightRight => vec![0],
        TurnDirection::Left | TurnDirection::UTurn => vec![lanes - 1],
        _ => (0..lanes).collect(),
    }
}

/// Which departure lane a movement from approach lane `k` enters, given every approach
/// lane (`sharing`, ascending) that makes the same movement.
///
/// Turning lanes are paired off from the kerb they turn towards: the rightmost
/// right-turning lane takes the rightmost departure lane, the next one the next, and the
/// same from the left for left turns and U-turns. Mapping every turning lane to the kerb
/// lane — the old rule — sent a double right turn (`turn:lanes=right|right;through|…`,
/// common on Manhattan's avenues) into one lane side by side, and the traffic auditor
/// found the pair colliding at the merge. Through lanes keep their own index, shifted
/// right only as far as the departure is narrower than the highest through lane needs;
/// only where there are more through lanes than departure lanes do two meet (a lane drop).
fn target_lane(turn: TurnDirection, k: usize, sharing: &[usize], out_lanes: usize) -> usize {
    let last = out_lanes - 1;
    match turn {
        TurnDirection::Right | TurnDirection::SlightRight => {
            sharing.iter().filter(|j| **j < k).count().min(last)
        }
        TurnDirection::Left | TurnDirection::UTurn => {
            last - sharing.iter().filter(|j| **j > k).count().min(last)
        }
        _ => {
            let highest = sharing.iter().copied().max().unwrap_or(k);
            let excess = highest.saturating_sub(last);
            k.saturating_sub(excess).min(last)
        }
    }
}

/// Builds the junctions, edges and lanes of the split-and-collapsed segment list.
#[allow(clippy::too_many_lines)]
fn build_network(
    segments: &[Segment],
    plans: &[WayPlan],
    file: &OsmFile,
    points: &[Vec3],
    options: &OsmOptions,
    report: &mut ImportReport,
) -> Result<Net> {
    // The drivable lane limits, split by where they came from, for the class-default
    // audit at the end (V4/W1). Vectors in lane order: no hashing, no set.
    let mut tagged_speeds: Vec<f64> = Vec::new();
    let mut defaulted_speeds: Vec<f64> = Vec::new();

    // --- junction ids, in ascending OSM node id order ---------------------------
    let mut junction_nodes: Vec<i64> = segments
        .iter()
        .flat_map(|s| [s.start_node(), s.end_node()])
        .collect();
    junction_nodes.sort_unstable();
    junction_nodes.dedup();
    let junction_of: BTreeMap<i64, JunctionId> = junction_nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (*node, JunctionId::new(i as u32)))
        .collect();

    // --- trimming radius per junction, per family --------------------------------
    // The motor radius is the widest carriageway that meets here, so a lane stops clear of
    // the crossing traffic. The soft radius is the widest *soft* way, so a footway is not
    // cut back by the width of the avenue it crosses.
    let mut motor_radius = vec![0.0f64; junction_nodes.len()];
    let mut soft_radius = vec![0.0f64; junction_nodes.len()];
    let mut motor_arms = vec![0usize; junction_nodes.len()];
    for segment in segments {
        let plan = &plans[segment.plan];
        let half = plan.half_width_m();
        for node in [segment.start_node(), segment.end_node()] {
            let j = junction_of[&node].as_usize();
            if plan.family == WayFamily::Motor {
                motor_radius[j] = motor_radius[j].max(half);
                motor_arms[j] += 1;
            } else {
                soft_radius[j] = soft_radius[j].max(half);
            }
        }
    }
    for (r, arms) in motor_radius.iter_mut().zip(&motor_arms) {
        if *arms >= 3 {
            *r += options.stop_line_setback_m.max(0.0);
        }
        *r = r.clamp(MIN_JUNCTION_RADIUS_M, MAX_JUNCTION_RADIUS_M);
    }
    for r in &mut soft_radius {
        *r = r.clamp(MIN_JUNCTION_RADIUS_M, MAX_SOFT_JUNCTION_RADIUS_M);
    }

    let mut net = Net {
        lanes: Vec::new(),
        edges: Vec::new(),
        junctions: junction_nodes
            .iter()
            .enumerate()
            .map(|(i, _node)| Junction {
                id: JunctionId::new(i as u32),
                position: Vec3::ZERO,
                shape: Vec::new(),
                incoming: Vec::new(),
                outgoing: Vec::new(),
                internal: Vec::new(),
                control: JunctionControl::Uncontrolled,
                conflicts: ConflictMatrix::new(0),
                // OSM node names are place labels, not junction names; the model's
                // junction name stays empty until a source supplies a real one.
                name: None,
            })
            .collect(),
        edge_info: Vec::new(),
        incoming: vec![Vec::new(); junction_nodes.len()],
        outgoing: vec![Vec::new(); junction_nodes.len()],
        movements: vec![Vec::new(); junction_nodes.len()],
        junction_nodes,
        junction_of,
    };
    // A junction's position is its OSM node's, when the extract carries that node. A
    // boundary junction created by the bounding-box clip has a synthetic negative id and
    // no node, so its position comes from the segment end that made it — which for a real
    // node is the same value, because a segment's geometry *is* its nodes projected.
    for i in 0..net.junction_nodes.len() {
        if let Some(index) = file.node_index(net.junction_nodes[i]) {
            net.junctions[i].position = points[index];
            continue;
        }
        for segment in segments {
            if segment.start_node() == net.junction_nodes[i] {
                net.junctions[i].position = segment.points[0];
                break;
            }
            if segment.end_node() == net.junction_nodes[i] {
                net.junctions[i].position = segment.points[segment.points.len() - 1];
                break;
            }
        }
    }

    // --- one or two edges per segment --------------------------------------------
    for (segment_index, segment) in segments.iter().enumerate() {
        let plan = &plans[segment.plan];
        let base = match options.import.simplify_tolerance_m {
            Some(tolerance) if tolerance > 0.0 => {
                let simplified = simplify_rdp(&segment.points, tolerance);
                if simplified.len() >= 2 {
                    simplified
                } else {
                    segment.points.clone()
                }
            }
            _ => segment.points.clone(),
        };
        let total = polyline_length(&base);
        let from_j = net.junction_of[&segment.start_node()];
        let to_j = net.junction_of[&segment.end_node()];
        let radius = if plan.family == WayFamily::Motor {
            &motor_radius
        } else {
            &soft_radius
        };
        let (cut_start, cut_end) = fit_trim(
            total,
            radius[from_j.as_usize()],
            radius[to_j.as_usize()],
            plan.osm_id,
            report,
        );
        let core = trim_polyline(&base, cut_start, total - cut_end).unwrap_or_else(|| base.clone());
        if core.len() < 2 {
            report.note(Anomaly::DegenerateLane, plan.osm_id);
            continue;
        }
        let mut reversed = core.clone();
        reversed.reverse();

        let half_width = plan.half_width_m();
        for (direction_lanes, geometry, from, to, turns, near_way, far_way) in [
            (
                segment.fwd,
                &core,
                from_j,
                to_j,
                segment.turn_fwd.as_ref(),
                segment.start_way,
                segment.end_way,
            ),
            (
                segment.bwd,
                &reversed,
                to_j,
                from_j,
                segment.turn_bwd.as_ref(),
                segment.end_way,
                segment.start_way,
            ),
        ] {
            if direction_lanes == 0 {
                continue;
            }
            let edge_id = EdgeId::new(net.edges.len() as u32);
            let mut lane_ids = Vec::with_capacity(usize::from(direction_lanes));
            for k in 0..direction_lanes {
                let offset = -(half_width - (f64::from(k) + 0.5) * plan.lane_width_m);
                let (offset_points, repaired) = offset_polyline(geometry, offset);
                let mut centreline = dedupe_points(offset_points);
                // A tunnel runs below ground. Flat at z = 0 it ran *through* the buildings
                // above it — the FDR Drive under the United Nations, the Park Avenue, 1st
                // Avenue and Queens-Midtown tunnels — and every vehicle in one was drawn
                // inside a building. The junction connectors at its portals interpolate
                // z, so the descent is made there.
                // And a bridge over a road runs above it: at z = 0 the Park Avenue
                // Viaduct crossed the street under it at grade, and the auditor found the
                // cars on the two "colliding".
                if plan.levels_below > 0 || plan.levels_above > 0 {
                    let z = TUNNEL_LEVEL_DEPTH_M
                        * (f64::from(plan.levels_above) - f64::from(plan.levels_below));
                    for p in &mut centreline {
                        p.z = z;
                    }
                }
                if centreline.len() < 2 || polyline_length(&centreline) < MIN_KEPT_LANE_M {
                    report.note(Anomaly::DegenerateLane, plan.osm_id);
                    continue;
                }
                let length_m = polyline_length(&centreline);
                let lane_id = LaneId::new(net.lanes.len() as u32);
                match Lane::new(
                    lane_id,
                    edge_id,
                    None,
                    k,
                    plan.kind,
                    centreline,
                    plan.lane_width_m,
                    plan.speed_mps,
                    plan.allowed,
                ) {
                    Ok(lane) => {
                        net.lanes.push(lane);
                        lane_ids.push(lane_id);
                        if repaired {
                            report.note(Anomaly::SelfIntersectingLane, plan.osm_id);
                            report.counts.lanes_repaired += 1;
                        }
                        if plan.family == WayFamily::Motor {
                            if plan.speed_from_preset {
                                report.counts.speeds_defaulted += 1;
                                defaulted_speeds.push(plan.speed_mps);
                            } else {
                                report.counts.speeds_tagged += 1;
                                tagged_speeds.push(plan.speed_mps);
                            }
                            match plan.width_source {
                                WidthSource::PerLaneTag | WidthSource::WayTag => {
                                    report.counts.widths_tagged += 1;
                                }
                                WidthSource::Option | WidthSource::Class => {
                                    report.counts.widths_defaulted += 1;
                                }
                            }
                            // V8: a junction-trimming sliver is kept, because dropping it
                            // disconnects whatever is on the other side, but it is named
                            // so a consumer can filter it rather than discover it.
                            if length_m < options.min_useful_lane_m {
                                report.note(Anomaly::ShortDrivingLane, plan.osm_id);
                                report.counts.short_driving_lanes += 1;
                            }
                        }
                    }
                    Err(_) => report.note(Anomaly::DegenerateLane, plan.osm_id),
                }
            }
            if lane_ids.is_empty() {
                report.note(Anomaly::EmptyEdgeDropped, plan.osm_id);
                continue;
            }
            net.junctions[to.as_usize()]
                .incoming
                .extend(lane_ids.iter().copied());
            net.junctions[from.as_usize()]
                .outgoing
                .extend(lane_ids.iter().copied());
            net.edges.push(Edge {
                id: edge_id,
                from,
                to,
                lanes: lane_ids.clone(),
                name: plan.name,
                road_class: plan.class,
            });
            let info = EdgeInfo {
                plan: segment.plan,
                segment: segment_index,
                family: plan.family,
                lanes: lane_ids,
                turns: turns.cloned(),
                near_way,
                far_way,
            };
            let index = net.edge_info.len();
            net.incoming[to.as_usize()].push(index);
            net.outgoing[from.as_usize()].push(index);
            net.edge_info.push(info);
        }

        if options.sidewalks_from_tags && plan.family == WayFamily::Motor {
            add_tagged_sidewalks(
                &mut net,
                plan,
                segment,
                segment_index,
                &base_for_sidewalks(&base, &soft_radius, from_j, to_j, plan.osm_id, report),
                (from_j, to_j),
                options,
                report,
            );
        }
    }

    // V4/W1: what the preset supplied, against what the source itself states.
    report.audit_speed_defaults(&mut tagged_speeds, &defaulted_speeds);

    Ok(net)
}

/// The centreline a tagged sidewalk is offset from: the segment re-trimmed with the **soft**
/// junction radius, because a pavement stops at the kerb rather than at the carriageway's
/// stop line.
fn base_for_sidewalks(
    base: &[Vec3],
    soft_radius: &[f64],
    from: JunctionId,
    to: JunctionId,
    osm_id: i64,
    report: &mut ImportReport,
) -> Vec<Vec3> {
    let total = polyline_length(base);
    let (cut_start, cut_end) = fit_trim(
        total,
        soft_radius[from.as_usize()],
        soft_radius[to.as_usize()],
        osm_id,
        report,
    );
    trim_polyline(base, cut_start, total - cut_end).unwrap_or_else(|| base.to_vec())
}

/// Materialises the pavements a road's `sidewalk=left|right|both` tag claims.
///
/// Off by default ([`OsmOptions::sidewalks_from_tags`]): a city that maps its pavements as
/// separate `footway` ways — which is what the pedestrian layer reads, and what the Phase 1
/// Manhattan extract does — would otherwise get two pavements for every one on the ground.
///
/// Each tagged side becomes a band of [`OsmOptions::sidewalk_width_m`] just outside the
/// carriageway, carrying one lane in each direction, the forward one on the right of the
/// band as right-hand traffic puts it. The lanes are [`LaneKind::Sidewalk`] and carry the
/// pedestrian class only, so no vehicle can route onto them.
#[allow(clippy::too_many_arguments)]
fn add_tagged_sidewalks(
    net: &mut Net,
    plan: &WayPlan,
    segment: &Segment,
    segment_index: usize,
    base: &[Vec3],
    junctions: (JunctionId, JunctionId),
    options: &OsmOptions,
    report: &mut ImportReport,
) {
    if base.len() < 2 {
        return;
    }
    let (from_j, to_j) = junctions;
    let mut reversed = base.to_vec();
    reversed.reverse();
    let width = options.sidewalk_width_m;
    let quarter = width * 0.25;
    for (side, present) in [(1.0, plan.sidewalk_left), (-1.0, plan.sidewalk_right)] {
        if !present {
            continue;
        }
        let band = side * (plan.half_width_m() + width * 0.5);
        for (geometry, offset, from, to) in [
            (base, band - quarter, from_j, to_j),
            (&reversed, -(band + quarter), to_j, from_j),
        ] {
            let (offset_points, repaired) = offset_polyline(geometry, offset);
            let centreline = dedupe_points(offset_points);
            if centreline.len() < 2 || polyline_length(&centreline) < MIN_KEPT_LANE_M {
                report.note(Anomaly::DegenerateLane, plan.osm_id);
                continue;
            }
            let edge_id = EdgeId::new(net.edges.len() as u32);
            let lane_id = LaneId::new(net.lanes.len() as u32);
            let Ok(lane) = Lane::new(
                lane_id,
                edge_id,
                None,
                0,
                LaneKind::Sidewalk,
                centreline,
                width * 0.5,
                WALKING_SPEED_MPS,
                ClassMask::PEDESTRIAN,
            ) else {
                report.note(Anomaly::DegenerateLane, plan.osm_id);
                continue;
            };
            net.lanes.push(lane);
            if repaired {
                report.note(Anomaly::SelfIntersectingLane, plan.osm_id);
                report.counts.lanes_repaired += 1;
            }
            net.junctions[to.as_usize()].incoming.push(lane_id);
            net.junctions[from.as_usize()].outgoing.push(lane_id);
            net.edges.push(Edge {
                id: edge_id,
                from,
                to,
                lanes: vec![lane_id],
                name: plan.name,
                road_class: RoadClass::Footway,
            });
            let index = net.edge_info.len();
            net.incoming[to.as_usize()].push(index);
            net.outgoing[from.as_usize()].push(index);
            net.edge_info.push(EdgeInfo {
                plan: segment.plan,
                segment: segment_index,
                family: WayFamily::Foot,
                lanes: vec![lane_id],
                turns: None,
                near_way: segment.start_way,
                far_way: segment.end_way,
            });
        }
    }
}

/// The speed limit given to a pavement lane, m/s — a walking pace, as the `footway` row of
/// [`HIGHWAY_TABLE`] uses.
const WALKING_SPEED_MPS: f64 = 1.39;

/// How far back from each end a segment is trimmed, given the two junction radii.
///
/// A segment shorter than both radii plus [`MIN_LANE_LENGTH_M`] cannot give both junctions
/// the room they asked for; the two cuts are then scaled down together, which keeps the
/// lane centred on what is left. A segment too short even for that is left untrimmed and
/// counted: its lanes overlap the junction polygons, which is ugly but connected, and
/// dropping it would tear a hole in the network.
fn fit_trim(
    total_m: f64,
    start_m: f64,
    end_m: f64,
    osm_id: i64,
    report: &mut ImportReport,
) -> (f64, f64) {
    let want = start_m + end_m;
    if total_m > want + MIN_LANE_LENGTH_M {
        return (start_m, end_m);
    }
    let room = total_m - MIN_LANE_LENGTH_M;
    if room <= 0.0 || want <= 0.0 {
        report.note(Anomaly::SegmentTooShortToTrim, osm_id);
        return (0.0, 0.0);
    }
    let scale = room / want;
    (start_m * scale, end_m * scale)
}

/// Builds every movement through every junction: its connector lane, its turn direction
/// and which approach lane may make it.
///
/// # The rule, in order
///
/// 1. Candidate departures are every outgoing motor edge except the opposite carriageway
///    of the approach's own segment, which would be a U-turn; that one is admitted only
///    when there is nothing else to do, so a dead end can be turned around in.
/// 2. The turn direction is the geometry's: the heading change from the approach lane's
///    end to the departure lane's start, banded by [`TurnDirection::from_heading_change`].
/// 3. `turn:lanes`, where the way carries a usable one, decides which lanes may make which
///    movement ([`turn_matches`]). Where it does not, the geometric rule applies: right
///    turns from the rightmost lane, left turns and U-turns from the leftmost, everything
///    else from any lane ([`lanes_for_turn`]).
/// 4. Any approach lane left with no movement at all is given the straightest candidate,
///    so that a lane is never a dead end because of a tag.
///
/// A movement whose two lane ends are within a millimetre of each other — which happens
/// when a segment was too short to trim — gets no connector lane, because [`Lane::new`]
/// refuses a sub-millimetre segment. It is returned as a [`DirectMovement`] so that the
/// restriction machinery still sees it (R3).
#[allow(clippy::too_many_lines)]
fn build_movements(
    net: &mut Net,
    plans: &[WayPlan],
    report: &mut ImportReport,
) -> Vec<DirectMovement> {
    let mut direct: Vec<DirectMovement> = Vec::new();
    for j in 0..net.junctions.len() {
        let approaches: Vec<usize> = net.incoming[j]
            .iter()
            .copied()
            .filter(|i| net.edge_info[*i].family == WayFamily::Motor)
            .collect();
        let departures: Vec<usize> = net.outgoing[j]
            .iter()
            .copied()
            .filter(|i| net.edge_info[*i].family == WayFamily::Motor)
            .collect();
        if approaches.is_empty() || departures.is_empty() {
            continue;
        }
        let internal_edge = EdgeId::new(net.edges.len() as u32);
        let mut movements: Vec<Movement> = Vec::new();
        let mut internal_lanes: Vec<LaneId> = Vec::new();

        for &a in &approaches {
            let approach = net.edge_info[a].clone();
            let in_lanes = approach.lanes.clone();
            let approach_heading = {
                let lane = &net.lanes[in_lanes[0].as_usize()];
                lane.heading_at(lane.length_m)
            };
            // Candidate departures, with the turn each one represents.
            let mut candidates: Vec<(usize, TurnDirection, f64)> = Vec::new();
            for &b in &departures {
                let departure = &net.edge_info[b];
                let out_lane = &net.lanes[departure.lanes[0].as_usize()];
                let delta = normalise_angle(out_lane.heading_at(0.0) - approach_heading);
                let turn = if departure.segment == approach.segment {
                    TurnDirection::UTurn
                } else {
                    TurnDirection::from_heading_change(delta)
                };
                candidates.push((b, turn, delta));
            }
            let has_non_uturn = candidates
                .iter()
                .any(|(b, _, _)| net.edge_info[*b].segment != approach.segment);
            if has_non_uturn {
                candidates.retain(|(b, _, _)| net.edge_info[*b].segment != approach.segment);
            }
            if candidates.is_empty() {
                continue;
            }

            let permits = |k: usize, turn: TurnDirection| -> bool {
                match approach.turns.as_ref().and_then(|t| t.get(k)) {
                    Some(set) if !set.is_empty() => set.iter().any(|t| turn_matches(*t, turn)),
                    _ => lanes_for_turn(turn, in_lanes.len()).contains(&k),
                }
            };
            for (k, &from_lane) in in_lanes.iter().enumerate() {
                let mut made = 0usize;
                let mut seen: Vec<LaneId> = Vec::new();
                for &(b, turn, _) in &candidates {
                    let departure = &net.edge_info[b];
                    let out_lanes = departure.lanes.len();
                    if !permits(k, turn) {
                        continue;
                    }
                    let sharing: Vec<usize> =
                        (0..in_lanes.len()).filter(|j| permits(*j, turn)).collect();
                    let to_lane = departure.lanes[target_lane(turn, k, &sharing, out_lanes)];
                    if seen.contains(&to_lane) {
                        continue;
                    }
                    seen.push(to_lane);
                    made += 1;
                    add_movement(
                        net,
                        &mut movements,
                        &mut internal_lanes,
                        &mut direct,
                        report,
                        internal_edge,
                        j,
                        MovementRequest {
                            from_lane,
                            to_lane,
                            turn,
                            approach_heading,
                            from_edge: a,
                            to_edge: b,
                            plan: approach.plan,
                            plans,
                        },
                    );
                }
                if made == 0 {
                    // Give the lane the straightest candidate rather than leave it a dead
                    // end: a tag should never disconnect a lane from the network.
                    let best = candidates
                        .iter()
                        .min_by(|x, y| x.2.abs().total_cmp(&y.2.abs()))
                        .copied();
                    if let Some((b, turn, _)) = best {
                        let departure = &net.edge_info[b];
                        let to_lane = departure.lanes[target_lane(turn, k, &[k], departure.lanes.len())];
                        add_movement(
                            net,
                            &mut movements,
                            &mut internal_lanes,
                            &mut direct,
                            report,
                            internal_edge,
                            j,
                            MovementRequest {
                                from_lane,
                                to_lane,
                                turn,
                                approach_heading,
                                from_edge: a,
                                to_edge: b,
                                plan: approach.plan,
                                plans,
                            },
                        );
                    }
                }
            }
        }

        if !internal_lanes.is_empty() {
            net.edges.push(Edge {
                id: internal_edge,
                from: JunctionId::new(j as u32),
                to: JunctionId::new(j as u32),
                lanes: internal_lanes.clone(),
                name: None,
                road_class: RoadClass::Internal,
            });
        }
        net.junctions[j].internal = internal_lanes;
        net.movements[j] = movements;
    }
    direct
}

/// The arguments of [`add_movement`], grouped so the function takes one parameter block
/// rather than nine.
struct MovementRequest<'a> {
    from_lane: LaneId,
    to_lane: LaneId,
    turn: TurnDirection,
    approach_heading: f64,
    from_edge: usize,
    to_edge: usize,
    plan: usize,
    plans: &'a [WayPlan],
}

/// Creates one movement's connector lane and records the movement.
#[allow(clippy::too_many_arguments)]
fn add_movement(
    net: &mut Net,
    movements: &mut Vec<Movement>,
    internal_lanes: &mut Vec<LaneId>,
    direct: &mut Vec<DirectMovement>,
    report: &mut ImportReport,
    internal_edge: EdgeId,
    junction: usize,
    request: MovementRequest<'_>,
) {
    let plan = &request.plans[request.plan];
    let (start, heading_in) = {
        let lane = &net.lanes[request.from_lane.as_usize()];
        (lane.end(), lane.heading_at(lane.length_m))
    };
    let (end, heading_out) = {
        let lane = &net.lanes[request.to_lane.as_usize()];
        (lane.start(), lane.heading_at(0.0))
    };
    let touching = |direct: &mut Vec<DirectMovement>, report: &mut ImportReport| {
        report.note(Anomaly::ZeroLengthConnector, plan.osm_id);
        direct.push(DirectMovement {
            from_lane: request.from_lane,
            to_lane: request.to_lane,
            turn: request.turn,
            junction,
            from_edge: request.from_edge,
            to_edge: request.to_edge,
            permitted: true,
        });
    };
    // A connector is built whenever `Lane::new` will take one — that is, whenever the two
    // lane ends are at least a millimetre apart. The old threshold was a whole metre, and
    // every movement below it left the restriction and conflict machinery entirely (R3).
    if start.distance_2d(end) < 2.0 * Q_POSITION_M {
        touching(direct, report);
        return;
    }
    let geometry = dedupe_points(connector_geometry(start, heading_in, end, heading_out));
    if geometry.len() < 2 {
        touching(direct, report);
        return;
    }
    let lane_id = LaneId::new(net.lanes.len() as u32);
    let index = u8::try_from(internal_lanes.len()).unwrap_or(u8::MAX);
    let Ok(lane) = Lane::new(
        lane_id,
        internal_edge,
        Some(JunctionId::new(junction as u32)),
        index,
        LaneKind::Internal,
        geometry,
        plan.lane_width_m,
        plan.speed_mps,
        plan.allowed,
    ) else {
        touching(direct, report);
        return;
    };
    net.lanes.push(lane);
    internal_lanes.push(lane_id);
    movements.push(Movement {
        from_lane: request.from_lane,
        to_lane: request.to_lane,
        internal: lane_id,
        turn: request.turn,
        approach_heading: request.approach_heading,
        from_edge: request.from_edge,
        to_edge: request.to_edge,
        permitted: true,
    });
}

/// Connects the pedestrian and cycle lanes that meet at each junction.
///
/// Soft lanes get **direct** lane-to-lane connections with no connector lane and no row in
/// the conflict matrix: a pedestrian crossing a junction is governed by the [`Crossing`]
/// records and by the VRU model, not by a junction-internal geometry the model would then
/// have to give right of way to. Foot connects to foot and cycle to cycle; a cyclist
/// changing to a footway dismounts, which is a mobility decision rather than a lane graph
/// one.
fn connect_soft_lanes(net: &Net) -> Vec<Connection> {
    let mut out = Vec::new();
    for j in 0..net.junctions.len() {
        for &a in &net.incoming[j] {
            let approach = &net.edge_info[a];
            if approach.family == WayFamily::Motor {
                continue;
            }
            let reversals = net.outgoing[j]
                .iter()
                .filter(|b| net.edge_info[**b].family == approach.family)
                .count();
            for &b in &net.outgoing[j] {
                let departure = &net.edge_info[b];
                if departure.family != approach.family {
                    continue;
                }
                // Turning back along the way one arrived on is only useful at a dead end.
                if departure.segment == approach.segment && reversals > 1 {
                    continue;
                }
                for &from_lane in &approach.lanes {
                    for &to_lane in &departure.lanes {
                        let delta = normalise_angle(
                            net.lanes[to_lane.as_usize()].heading_at(0.0)
                                - net.lanes[from_lane.as_usize()]
                                    .heading_at(net.lanes[from_lane.as_usize()].length_m),
                        );
                        out.push(Connection {
                            from_lane,
                            to_lane,
                            via: None,
                            direction: TurnDirection::from_heading_change(delta),
                            permitted: true,
                        });
                    }
                }
            }
        }
    }
    out
}

/// What a `restriction=*` value forbids, and which turn it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Restriction {
    /// Whether it forbids the named movement or forbids everything else.
    kind: RestrictionKind,
    /// The turn the tag names, when it names one.
    ///
    /// Throwing this away made every `no_*` a blanket ban on the `from` × `to` product,
    /// so one `no_left_turn` banned a legal right turn from the opposite approach of the
    /// same street (R4).
    turn: Option<TurnDirection>,
}

/// Which way a [`Restriction`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestrictionKind {
    /// `no_*`: the `from` → `to` movement it names is forbidden.
    ///
    /// This covers `no_entry` and `no_exit` too. The OSM wiki describes them as the forms
    /// that take several `to` members and several `from` members respectively, which is
    /// exactly what a `from` set crossed with a `to` set already expresses; there is no
    /// third rule to implement. Neither names a turn.
    Forbid,
    /// `only_*`: every movement from `from` except the one it names is forbidden.
    Only,
}

/// Parses a `restriction=*` value into its direction and the turn it names.
///
/// An unrecognised suffix keeps the direction and names no turn, which is the old
/// behaviour: a `no_*` this importer has never heard of should still ban something rather
/// than nothing.
fn parse_restriction(value: &str) -> Option<Restriction> {
    let v = value.trim();
    let (kind, rest) = match v.strip_prefix("no_") {
        Some(rest) => (RestrictionKind::Forbid, rest),
        None => (RestrictionKind::Only, v.strip_prefix("only_")?),
    };
    let turn = match rest {
        "left_turn" => Some(TurnDirection::Left),
        "right_turn" => Some(TurnDirection::Right),
        "straight_on" => Some(TurnDirection::Straight),
        "u_turn" => Some(TurnDirection::UTurn),
        // `no_entry` and `no_exit` name a set of members, not a turn, and an unknown
        // suffix names nothing we can check.
        _ => None,
    };
    Some(Restriction { kind, turn })
}

/// Applies every `type=restriction` relation by marking the movements it forbids as not
/// permitted.
///
/// The geometry stays: [`Connection::permitted`] exists so that a banned turn can be shown
/// to a user and refused to a router in the same model. A router must therefore filter on
/// `permitted`, which is what "removing the connection" means here.
///
/// Only `via` **node** restrictions are applied. A `via` way — the long form used for a
/// restriction across a slip road — spans more than one junction and has no single
/// movement to ban; those are counted as unmatched rather than guessed at.
///
/// # What a movement has to match
///
/// Three things, all of them necessary (R4):
///
/// * the approach edge's **far** way — the one that reaches the via node — is a `from`
///   member. Matching the edge's whole way list instead banned the movement on every
///   approach that merely shared a way id, which on a street that passes through the
///   junction twice is the *opposite* approach making a legal turn;
/// * the departure edge's **near** way is a `to` member, by the same argument;
/// * the movement's own turn direction matches the one the tag names
///   ([`turn_matches`], which is deliberately loose: a mapper writes `left` for anything
///   leaving to the left and the junction may be skewed enough that the geometry calls it
///   a slight left).
///
/// Movements with no connector lane are in `direct` rather than in `net.movements`, and
/// are subject to exactly the same test — before this they escaped it entirely, so a
/// banned turn between two lanes that happened to touch stayed permitted (R3).
fn apply_restrictions(
    net: &mut Net,
    direct: &mut [DirectMovement],
    file: &OsmFile,
    report: &mut ImportReport,
) -> (u64, u64) {
    let mut applied = 0u64;
    let mut banned = 0u64;
    for relation in &file.relations {
        let Some(kind_tag) = relation.tags.get("type") else {
            continue;
        };
        if !kind_tag.starts_with("restriction") {
            continue;
        }
        // `restriction`, or a modal form such as `restriction:motorcar`.
        let value = relation
            .tags
            .get("restriction")
            .or_else(|| relation.tags.get("restriction:motorcar"))
            .or_else(|| relation.tags.get("restriction:motor_vehicle"));
        let Some(value) = value else {
            // A modal restriction this importer does not model (`restriction:hgv`,
            // `restriction:bicycle`, …), or a relation with no `restriction` tag at all.
            report.note(Anomaly::UnknownRestriction, relation.id);
            continue;
        };
        let Some(restriction) = parse_restriction(value) else {
            report.note(Anomaly::UnknownRestriction, relation.id);
            continue;
        };
        let mut from_ways: Vec<i64> = Vec::new();
        let mut to_ways: Vec<i64> = Vec::new();
        let mut via_node: Option<i64> = None;
        let mut via_way = false;
        for member in &relation.members {
            match (member.role.as_str(), member.kind) {
                ("from", MemberKind::Way) => from_ways.push(member.id),
                ("to", MemberKind::Way) => to_ways.push(member.id),
                ("via", MemberKind::Node) => via_node = Some(member.id),
                ("via", MemberKind::Way) => via_way = true,
                _ => {}
            }
        }
        if from_ways.is_empty() || to_ways.is_empty() {
            report.note(Anomaly::RestrictionIncomplete, relation.id);
            continue;
        }
        let Some(via) = via_node else {
            if via_way {
                report.note(Anomaly::RestrictionUnmatched, relation.id);
            } else {
                report.note(Anomaly::RestrictionIncomplete, relation.id);
            }
            continue;
        };
        let Some(junction) = net.junction_of.get(&via).copied() else {
            report.note(Anomaly::RestrictionUnmatched, relation.id);
            continue;
        };
        let mut hit = 0u64;
        let j = junction.as_usize();
        let forbids = |net: &Net, from_edge: usize, to_edge: usize, turn: TurnDirection| -> bool {
            let from_matches = from_ways.contains(&net.edge_info[from_edge].far_way);
            let to_matches = to_ways.contains(&net.edge_info[to_edge].near_way);
            let turn_ok = restriction.turn.is_none_or(|t| turn_matches(t, turn));
            match restriction.kind {
                RestrictionKind::Forbid => from_matches && to_matches && turn_ok,
                RestrictionKind::Only => from_matches && !(to_matches && turn_ok),
            }
        };
        for index in 0..net.movements[j].len() {
            let movement = net.movements[j][index];
            if forbids(net, movement.from_edge, movement.to_edge, movement.turn)
                && net.movements[j][index].permitted
            {
                net.movements[j][index].permitted = false;
                hit += 1;
            }
        }
        for movement in direct.iter_mut().filter(|m| m.junction == j) {
            if movement.permitted
                && forbids(net, movement.from_edge, movement.to_edge, movement.turn)
            {
                movement.permitted = false;
                hit += 1;
            }
        }
        if hit == 0 {
            report.note(Anomaly::RestrictionNoMovement, relation.id);
        } else {
            applied += 1;
            banned += hit;
        }
    }
    (applied, banned)
}

/// Every node named as the `via` of a turn restriction.
///
/// These nodes must survive the trivial-junction collapse: a restriction whose junction was
/// merged away has nowhere to apply, and would be silently lost rather than silently
/// obeyed.
fn restriction_via_nodes(file: &OsmFile) -> BTreeSet<i64> {
    let mut out = BTreeSet::new();
    for relation in &file.relations {
        if !relation
            .tags
            .get("type")
            .is_some_and(|t| t.starts_with("restriction"))
        {
            continue;
        }
        for member in &relation.members {
            if member.kind == MemberKind::Node && member.role == "via" {
                out.insert(member.id);
            }
        }
    }
    out
}

/// The conflict matrix of one junction, from its movements.
///
/// The rule is the one [`crate::procedural`] documents and uses, generalised from the
/// four cardinal directions to arbitrary headings:
///
/// * two movements are **foes** when they end on the same lane (a merge) or when their
///   connector polylines cross; two movements that *start* on the same lane are a
///   divergence and never conflict;
/// * of two conflicting movements, the one that crosses opposing traffic (a left turn or a
///   U-turn, in right-hand traffic) gives way to the one that does not;
/// * of two of equal rank, the one whose conflicting partner approaches **from its right**
///   gives way — `b` is on `a`'s right when the heading change from `a`'s approach to
///   `b`'s lies in `(0, π)`;
/// * a pair the rules leave level — two opposing left turns — is recorded as a conflict
///   with no precedence either way, because no highway code settles it and inventing one
///   would be worse than telling the intersection model that it must.
fn conflict_matrix(movements: &[Movement], lanes: &[Lane]) -> ConflictMatrix {
    let mut matrix = ConflictMatrix::new(movements.len());
    let rank = |m: &Movement| u8::from(!m.turn.crosses_opposing_traffic());
    let bbox = |m: &Movement| {
        let points = &lanes[m.internal.as_usize()].centreline;
        let mut min = points[0];
        let mut max = points[0];
        for p in points {
            min = Vec3::new(min.x.min(p.x), min.y.min(p.y), 0.0);
            max = Vec3::new(max.x.max(p.x), max.y.max(p.y), 0.0);
        }
        (min, max)
    };
    let boxes: Vec<(Vec3, Vec3)> = movements.iter().map(bbox).collect();
    for a in 0..movements.len() {
        for b in a + 1..movements.len() {
            let (ma, mb) = (&movements[a], &movements[b]);
            if ma.from_lane == mb.from_lane {
                continue;
            }
            let merges = ma.to_lane == mb.to_lane;
            let separated = boxes[a].1.x < boxes[b].0.x
                || boxes[b].1.x < boxes[a].0.x
                || boxes[a].1.y < boxes[b].0.y
                || boxes[b].1.y < boxes[a].0.y;
            let crosses = !separated
                && crate::index::polylines_cross(
                    &lanes[ma.internal.as_usize()].centreline,
                    &lanes[mb.internal.as_usize()].centreline,
                );
            if !(merges || crosses) {
                continue;
            }
            matrix.set_foe(a, b, true);
            let (ra, rb) = (rank(ma), rank(mb));
            if ra < rb {
                matrix.set_response(a, b, true);
            } else if rb < ra {
                matrix.set_response(b, a, true);
            } else {
                let delta = normalise_angle(mb.approach_heading - ma.approach_heading);
                if delta > Q_ANGLE_RAD && delta < core::f64::consts::PI - Q_ANGLE_RAD {
                    matrix.set_response(a, b, true);
                } else if delta < -Q_ANGLE_RAD && delta > -core::f64::consts::PI + Q_ANGLE_RAD {
                    matrix.set_response(b, a, true);
                }
            }
        }
    }
    matrix
}

/// Sets each junction's control type from its tags and its shape.
///
/// A signalised junction is set later, by [`synthesise_signals`], because it needs a plan
/// to point at. Everything else is decided here: an explicit `highway=stop` or
/// `highway=give_way` node, a roundabout arm, and otherwise priority for a real junction
/// and no control for a place where two ways simply meet.
fn assign_control(net: &mut Net, plans: &[WayPlan], file: &OsmFile) {
    for j in 0..net.junctions.len() {
        let node = net.junction_nodes[j];
        let tags = file.tags_of_node(node);
        let roundabout = net.incoming[j]
            .iter()
            .chain(&net.outgoing[j])
            .any(|e| plans[net.edge_info[*e].plan].roundabout);
        let motor_approaches = net.incoming[j]
            .iter()
            .filter(|e| net.edge_info[**e].family == WayFamily::Motor)
            .count();
        let control = if roundabout {
            JunctionControl::Roundabout
        } else if tags.is_some_and(|t| t.is("highway", "stop")) {
            JunctionControl::Stop
        } else if tags.is_some_and(|t| t.is("highway", "give_way")) {
            JunctionControl::Yield
        } else if motor_approaches >= 2 && !net.movements[j].is_empty() {
            JunctionControl::Priority
        } else {
            JunctionControl::Uncontrolled
        };
        net.junctions[j].control = control;
    }
}

// ---------------------------------------------------------------------------
// Stage 8: signals
// ---------------------------------------------------------------------------

/// Which phase group a movement belongs to, given the junction's reference axis.
///
/// Two groups: the approaches parallel or anti-parallel to the first approach share the
/// green, everything else gets the other one. A skewed or five-arm junction therefore gets
/// a plan that is safe rather than efficient, which is the right trade for a plan the
/// source did not supply — 04-models.md §1.2 records that an OSM import gets signal
/// *presence* only.
fn phase_group(approach_heading: f64, reference: f64) -> u16 {
    let d = normalise_angle(approach_heading - reference).abs();
    let quarter = core::f64::consts::FRAC_PI_4;
    u16::from(!(d <= quarter || d >= 3.0 * quarter))
}

/// Synthesises a fixed-time plan for every junction that has a `traffic_signals` node on
/// it or near it, with the defaults of 04-models.md §2.3.
///
/// A signal node that is *not* a junction — a mid-block pedestrian signal, or one that the
/// trivial-junction collapse absorbed — is attached to the nearest junction within
/// [`ImportOptions::guess_signals_m`], which is what netconvert's `--tls.guess-signals`
/// does. One with no junction in range is counted as an orphan and ignored.
///
/// The amber time is the ITE formula `y = t + v / (2a)` on the fastest approach, clamped to
/// the FHWA range; the greens are then whatever is left of the target cycle, so that the
/// plan's phases sum to its cycle exactly as [`World::validate`] requires.
fn synthesise_signals(
    net: &mut Net,
    file: &OsmFile,
    points: &[Vec3],
    options: &OsmOptions,
    report: &mut ImportReport,
) -> Vec<SignalPlan> {
    let mut signal_nodes: Vec<i64> = Vec::new();
    for (node, tags) in &file.node_tags {
        if tags.is("highway", "traffic_signals") {
            signal_nodes.push(*node);
        }
    }
    report.counts.traffic_signal_nodes = signal_nodes.len() as u64;

    let mut signalised: BTreeSet<usize> = BTreeSet::new();
    let radius = options.import.guess_signals_m.max(0.0);
    for node in &signal_nodes {
        if let Some(j) = net.junction_of.get(node) {
            signalised.insert(j.as_usize());
            continue;
        }
        let Some(index) = file.node_index(*node) else {
            report.note(Anomaly::OrphanTrafficSignal, *node);
            continue;
        };
        let position = points[index];
        let mut best: Option<(usize, f64)> = None;
        for (j, junction) in net.junctions.iter().enumerate() {
            if net.movements[j].is_empty() {
                continue;
            }
            let d = junction.position.distance_2d(position);
            if d <= radius && best.is_none_or(|(_, b)| d < b) {
                best = Some((j, d));
            }
        }
        match best {
            Some((j, _)) => {
                signalised.insert(j);
            }
            None => report.note(Anomaly::OrphanTrafficSignal, *node),
        }
    }

    let defaults = &options.signals;
    let mut plans: Vec<SignalPlan> = Vec::new();
    for j in signalised {
        let movements = net.movements[j].clone();
        if movements.is_empty() {
            continue;
        }
        let reference = movements[0].approach_heading;
        let groups: Vec<u16> = movements
            .iter()
            .map(|m| phase_group(m.approach_heading, reference))
            .collect();
        let present: Vec<u16> = {
            let mut g = groups.clone();
            g.sort_unstable();
            g.dedup();
            g
        };
        let speed = movements
            .iter()
            .map(|m| net.lanes[m.from_lane.as_usize()].speed_limit_mps)
            .fold(0.0f64, f64::max);
        let yellow = quantise(
            (defaults.yellow_reaction_s + speed / (2.0 * defaults.yellow_min_decel_mps2))
                .clamp(defaults.yellow_min_s, defaults.yellow_max_s),
            0.1,
        );
        let all_red = quantise(defaults.all_red_s.max(0.0), Q_TIME_S);
        let fixed = (yellow + all_red) * present.len() as f64;
        let green = quantise(
            ((defaults.cycle_s - fixed) / present.len() as f64).max(defaults.min_green_s),
            Q_TIME_S,
        );

        let state_of = |active: u16, amber: bool, index: usize| -> SignalState {
            if groups[index] != active {
                SignalState::Red
            } else if amber {
                SignalState::Amber
            } else if movements[index].turn.crosses_opposing_traffic() {
                // Permissive green: go, giving way to the conflicting movements the
                // junction's matrix names.
                SignalState::GreenYield
            } else {
                SignalState::Green
            }
        };
        let mut phases: Vec<SignalPhase> = Vec::new();
        for active in &present {
            phases.push(SignalPhase {
                duration_s: green,
                states: (0..movements.len())
                    .map(|i| state_of(*active, false, i))
                    .collect(),
                name: None,
            });
            phases.push(SignalPhase {
                duration_s: yellow,
                states: (0..movements.len())
                    .map(|i| state_of(*active, true, i))
                    .collect(),
                name: None,
            });
            if all_red > 0.0 {
                phases.push(SignalPhase {
                    duration_s: all_red,
                    states: vec![SignalState::Red; movements.len()],
                    name: None,
                });
            }
        }
        // The plan's cycle is the sum of what it actually contains, not the target: a
        // plan whose phases do not fill its cycle is rejected by `World::validate`, and
        // rounding the amber to a tenth of a second makes that a real risk.
        let cycle = quantise(phases.iter().map(|p| p.duration_s).sum::<f64>(), Q_TIME_S);

        let mut heads: Vec<SignalHead> = Vec::new();
        let mut seen: Vec<LaneId> = Vec::new();
        for (index, movement) in movements.iter().enumerate() {
            if seen.contains(&movement.from_lane) {
                continue;
            }
            seen.push(movement.from_lane);
            let lane = &net.lanes[movement.from_lane.as_usize()];
            let end = lane.end();
            heads.push(SignalHead {
                lane: movement.from_lane,
                position: Vec3::new(end.x, end.y, end.z + defaults.head_height_m),
                kind: SignalHeadKind::Vehicle,
                group: groups[index],
            });
        }

        let id = SignalId::new(plans.len() as u32);
        plans.push(SignalPlan {
            id,
            junction: JunctionId::new(j as u32),
            cycle_s: cycle,
            offset_s: 0.0,
            controlled: movements.iter().map(|m| m.internal).collect(),
            phases,
            heads,
        });
        net.junctions[j].control = JunctionControl::Signalised { plan: id };
    }
    plans
}

// ---------------------------------------------------------------------------
// Stage 9: crossings
// ---------------------------------------------------------------------------

/// Imports `footway=crossing` (and `cycleway=crossing`) ways as [`Crossing`] records.
///
/// A crossing belongs to the junction nearest its midpoint, within
/// [`OsmOptions::crossing_snap_m`]; one that belongs to no junction is counted and
/// dropped, because the model has nowhere to hang it. `crossing=zebra`, `marked` and
/// `uncontrolled` give the pedestrian priority; `unmarked` and `traffic_signals` do not.
fn build_crossings(
    file: &OsmFile,
    points: &[Vec3],
    net: &Net,
    options: &OsmOptions,
    report: &mut ImportReport,
) -> Vec<Crossing> {
    let mut out: Vec<(JunctionId, i64, Crossing)> = Vec::new();
    for way in &file.ways {
        let is_crossing = way.tags.is("footway", "crossing")
            || way.tags.is("cycleway", "crossing")
            || way.tags.is("path", "crossing")
            || (way.tags.has("crossing") && way.tags.has("highway"));
        if !is_crossing {
            continue;
        }
        let resolved: Vec<Vec3> = way
            .nodes
            .iter()
            .filter_map(|n| file.node_index(*n))
            .map(|i| points[i])
            .collect();
        if resolved.len() < 2 {
            report.note(Anomaly::WayTooShort, way.id);
            continue;
        }
        let from = resolved[0];
        let to = resolved[resolved.len() - 1];
        if from.distance_2d(to) < MIN_LANE_LENGTH_M {
            continue;
        }
        let midpoint = from.lerp(to, 0.5);
        let mut best: Option<(JunctionId, f64)> = None;
        for junction in &net.junctions {
            let d = junction.position.distance_2d(midpoint);
            if d <= options.crossing_snap_m && best.is_none_or(|(_, b)| d < b) {
                best = Some((junction.id, d));
            }
        }
        let Some((junction, _)) = best else {
            report.note(Anomaly::CrossingWithoutJunction, way.id);
            continue;
        };
        let width_m = match way.tags.get("width").map(parse_measure) {
            Some(Some(measure)) => {
                if measure.multi_valued {
                    report.note(Anomaly::MultiValuedHeight, way.id);
                }
                if measure.metres > 0.0 && measure.metres < 50.0 {
                    measure.metres
                } else {
                    options.crossing_width_m
                }
            }
            Some(None) => {
                report.note(Anomaly::UnparsableHeight, way.id);
                options.crossing_width_m
            }
            None => options.crossing_width_m,
        };
        let priority = matches!(
            way.tags.get("crossing").unwrap_or("unmarked"),
            "zebra" | "marked" | "uncontrolled"
        );
        out.push((
            junction,
            way.id,
            Crossing {
                id: CrossingId::new(0),
                junction,
                from,
                to,
                width_m,
                priority,
            },
        ));
    }
    out.sort_by_key(|(junction, way, _)| (junction.index(), *way));
    out.into_iter()
        .enumerate()
        .map(|(i, (_, _, mut crossing))| {
            crossing.id = CrossingId::new(i as u32);
            crossing
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stage 9 (continued): buildings and land use
// ---------------------------------------------------------------------------

/// Resolves a way's node references to projected points, skipping any the extract does not
/// carry, and returns `None` if fewer than three distinct points survive.
fn resolve_ring(nodes: &[i64], file: &OsmFile, points: &[Vec3]) -> Option<Vec<Vec3>> {
    let ring: Vec<Vec3> = nodes
        .iter()
        .filter_map(|n| file.node_index(*n))
        .map(|i| points[i])
        .collect();
    let ring = dedupe_points(ring);
    (ring.len() >= 3).then_some(ring)
}

/// Chains a multipolygon's member ways into closed rings of node ids.
///
/// A multipolygon's outer boundary is often split across several ways, in no particular
/// order and in either direction; the OSM specification only promises that they join end
/// to end. Members are taken in way-id order, so the result does not depend on the order
/// the relation happens to list them in, and a fragment that cannot be closed is dropped
/// with an [`Anomaly::UnclosedRing`] rather than imported as a ragged polygon.
fn chain_rings(
    mut pool: Vec<(i64, Vec<i64>)>,
    relation_id: i64,
    report: &mut ImportReport,
) -> Vec<Vec<i64>> {
    pool.sort_by_key(|(id, _)| *id);
    let mut used = vec![false; pool.len()];
    let mut rings: Vec<Vec<i64>> = Vec::new();
    for seed in 0..pool.len() {
        if used[seed] || pool[seed].1.len() < 2 {
            used[seed] = true;
            continue;
        }
        used[seed] = true;
        let mut ring = pool[seed].1.clone();
        loop {
            if ring.first() == ring.last() && ring.len() >= 4 {
                break;
            }
            let tail = *ring.last().expect("a seeded ring is never empty");
            let mut joined = false;
            for i in 0..pool.len() {
                if used[i] || pool[i].1.len() < 2 {
                    continue;
                }
                let candidate = &pool[i].1;
                if candidate[0] == tail {
                    ring.extend_from_slice(&candidate[1..]);
                } else if candidate[candidate.len() - 1] == tail {
                    ring.extend(candidate.iter().rev().skip(1).copied());
                } else {
                    continue;
                }
                used[i] = true;
                joined = true;
                break;
            }
            if !joined {
                break;
            }
        }
        if ring.first() == ring.last() && ring.len() >= 4 {
            rings.push(ring);
        } else {
            report.note(Anomaly::UnclosedRing, relation_id);
        }
    }
    rings
}

/// Where a building's height came from, and what it is.
struct HeightDecision {
    height_m: f64,
    min_height_m: f64,
    levels: Option<u16>,
    source: HeightSource,
    /// True when the tagged height was a spire or mast tip and was cut back to the
    /// structural top (V10).
    spire_capped: bool,
}

/// The largest height this importer will believe, metres. The tallest building on Earth is
/// 828 m; anything above a kilometre is a typo or a unit error.
const MAX_BUILDING_HEIGHT_M: f64 = 1000.0;

/// Applies the height rule of 04-models.md §1.3 to one building's tags.
///
/// In order: an explicit `height`; else `building:levels × metres_per_level`; else the
/// land-use default. Which rule fired is recorded in [`HeightSource`], so that a later
/// calibration of `metres_per_level` can be applied to exactly the buildings that used it
/// — which is the whole reason the field exists (invariant I-W3).
///
/// Both `metres_per_level` and `default_height_m` are **`TODO: calibrate`**: the OSM wiki
/// states no standard level-to-metre factor, and 04-models.md §1.3's plan is to fit both by
/// regressing Microsoft-estimated heights on OSM `building:levels` over the Phase 2 city
/// boxes.
fn decide_height(
    tags: &Tags,
    element: i64,
    options: &OsmOptions,
    report: &mut ImportReport,
) -> HeightDecision {
    let measure = |key: &str, report: &mut ImportReport| -> Option<f64> {
        let raw = tags.get(key)?;
        match parse_measure(raw) {
            Some(measure) => {
                if measure.multi_valued {
                    report.note(Anomaly::MultiValuedHeight, element);
                }
                Some(measure.metres)
            }
            None => {
                report.note(Anomaly::UnparsableHeight, element);
                None
            }
        }
    };
    let levels = match tags.get("building:levels") {
        Some(raw) => match parse_count(raw) {
            Some(v) => Some(v.min(u32::from(u16::MAX)) as u16),
            None => {
                report.note(Anomaly::UnparsableLevels, element);
                None
            }
        },
        None => None,
    };
    let tagged = measure("height", report).filter(|h| {
        let ok = *h > 0.0 && *h <= MAX_BUILDING_HEIGHT_M;
        if !ok {
            report.note(Anomaly::ImplausibleHeight, element);
        }
        ok
    });
    let (mut height_m, source) = match (tagged, levels) {
        (Some(h), _) => (h, HeightSource::Tagged),
        (None, Some(levels)) if levels > 0 => (
            f64::from(levels) * options.import.metres_per_level,
            HeightSource::FromLevels,
        ),
        _ => (options.import.default_height_m, HeightSource::Defaulted),
    };
    let min_height_m = measure("min_height", report)
        .or_else(|| {
            tags.get("building:min_level")
                .and_then(parse_count)
                .map(|l| f64::from(l) * options.import.metres_per_level)
        })
        .unwrap_or(0.0)
        .clamp(0.0, height_m);
    // V10: `height` in Simple 3D Buildings runs to the **tip**, and `roof:height` is how
    // much of it is roof. A gabled or hipped roof is building mass and stays; a roof that
    // tapers to a point is a spire or a mast, and an obstacle model handed it as a
    // full-height prism reports blockage from an antenna. The Phase 1 world's tallest
    // "building" was exactly that.
    //
    // Two triggers, neither of them invented: the `roof:shape` vocabulary of OSM Simple 3D
    // Buildings names the shapes that taper, and a roof taller than
    // `roof_spire_fraction` of the whole building is not a roof whatever it is called.
    let mut spire_capped = false;
    if let Some(roof) = measure("roof:height", report) {
        let structural = height_m - roof;
        let pointed = matches!(
            tags.get("roof:shape").unwrap_or(""),
            "spire" | "cone" | "pyramidal" | "onion"
        );
        // `>=` for the same reason as the guard below: a roof that is exactly half the
        // building is as tall as everything under it, and "half" is a value a mapper
        // writes on purpose (`height=100`, `roof:height=50`).
        let mostly_roof = roof >= options.roof_spire_fraction * height_m;
        // `>=`, not `>`: the Empire State Building's spire is `height = 443.2`,
        // `roof:height = 113.2`, `min_height = 330`, so its structural top is exactly
        // 330.0 m — equal to its own base, because the whole volume above 330 m IS the
        // spire. A strict `>` failed on that equality and let the 443.2 m mast stand
        // (V10). A part whose structural top equals its base is entirely spire, and
        // cutting it back to its base is the right answer: it then has no mass above
        // the volume it sits on.
        if (pointed || mostly_roof) && structural >= min_height_m && structural > 0.0 {
            height_m = structural;
            spire_capped = true;
        }
    }
    HeightDecision {
        height_m,
        min_height_m,
        levels,
        source,
        spire_capped,
    }
}

/// The [`MaterialClass`] a `building:material` or `material` value maps to.
fn material_of(tags: &Tags) -> MaterialClass {
    let raw = tags
        .get("building:material")
        .or_else(|| tags.get("material"))
        .unwrap_or("");
    match raw {
        "concrete" | "reinforced_concrete" | "cement" => MaterialClass::Concrete,
        "brick" | "masonry" | "stone" | "sandstone" | "limestone" | "clay" => MaterialClass::Brick,
        "glass" | "mirror" => MaterialClass::Glass,
        "wood" | "timber" | "timber_framing" => MaterialClass::Wood,
        "metal" | "steel" | "aluminium" | "copper" | "zinc" => MaterialClass::Metal,
        _ => MaterialClass::Unknown,
    }
}

/// True if `tags` describe a building-shaped polygon of any kind — an outline, a part, or
/// a structure this importer will decide not to make an obstacle of.
///
/// This is the predicate that keeps a building out of the land-use layer and puts it into
/// the frame's extent scan. Whether it becomes an obstacle is [`building_role`]'s
/// decision, which is a different question.
fn is_building_polygon(tags: &Tags) -> bool {
    let building = tags.get("building").unwrap_or("no");
    let part = tags.get("building:part").unwrap_or("no");
    (building != "no" && !building.is_empty()) || (part != "no" && !part.is_empty())
}

/// What a building-shaped polygon is, for the obstacle set (V1, V2/W4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildingRole {
    /// A `building=*` outline: one real structure, and therefore one obstacle.
    Outline,
    /// A `building:part=*` volume.
    ///
    /// Under OSM *Simple 3D Buildings* a part **subdivides** the `building=*` outline it
    /// lies in; it does not add to it. Importing parts as buildings in their own right
    /// counted the Phase 1 world's structures 2.44× over (7 390 against about 3 023),
    /// gave the obstacle R-tree 2.4× the entries it needed, and made the tallest
    /// "building" in Midtown the Empire State Building's spire — a part with
    /// `height = 443.2` and `min_height = 330` (V1, V10).
    Part,
    /// Tagged as a building, but placed below ground or transit infrastructure rather
    /// than building mass: excluded from the obstacle set (V2/W4).
    Subsurface,
}

/// The `layer` tag as a signed integer, or 0.
fn layer_of(tags: &Tags) -> i32 {
    tags.get("layer")
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

/// True if the polygon's tags place it below the ground surface.
///
/// The keys are the ones OSM actually uses for this, and the Phase 1 extract's three
/// phantom obstacles carried all of them at once: way 812938420 (Times Square–42nd
/// Street) is `building=train_station`, `location=underground`, `layer=-1`,
/// `underground=yes`, `railway=station`. Imported as a building it became a 10 m prism
/// over the roadway, 205 361 m² of them over Times Square, Herald Square and Grand
/// Central — exactly the intersections a V2X study models NLOS propagation at.
fn is_subsurface(tags: &Tags) -> bool {
    if tags.is("location", "underground") || tags.is("underground", "yes") {
        return true;
    }
    if layer_of(tags) < 0 {
        return true;
    }
    // A negative `building:min_level` with no positive `height` is a basement outline.
    let min_level = tags
        .get("building:min_level")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    min_level < 0.0 && !tags.has("height")
}

/// True if the polygon is transit infrastructure rather than a building: it names a
/// station or a public-transport area and carries no `building` tag of its own, so there
/// is no above-ground mass for it to represent.
///
/// A station that really does have a head house carries `building=*` — Grand Central's
/// does — and is not caught here.
fn is_transit_only(tags: &Tags) -> bool {
    let transit = tags.has("public_transport")
        || matches!(
            tags.get("railway").unwrap_or(""),
            "station" | "halt" | "subway_entrance" | "platform"
        );
    let building = tags.get("building").unwrap_or("no");
    transit && (building == "no" || building.is_empty())
}

/// What one building-shaped polygon becomes, or `None` if it is not one.
fn building_role(tags: &Tags) -> Option<BuildingRole> {
    if !is_building_polygon(tags) {
        return None;
    }
    if is_subsurface(tags) || is_transit_only(tags) {
        return Some(BuildingRole::Subsurface);
    }
    let building = tags.get("building").unwrap_or("no");
    if building != "no" && !building.is_empty() {
        Some(BuildingRole::Outline)
    } else {
        Some(BuildingRole::Part)
    }
}

/// The side of one cell of the outline lookup grid, metres.
///
/// A building footprint is tens of metres across, so a 100 m cell holds a handful of
/// them and a part's containment test looks at one cell rather than at every outline in
/// the city. It is an index parameter: it changes how long the import takes and nothing
/// about what it produces.
const OUTLINE_CELL_M: f64 = 100.0;

/// The most cells one outline may occupy in the lookup grid before it is put on the
/// always-scanned list instead.
///
/// A ring that spans more than this is mistagged (a boundary relation imported as a
/// building); indexing it would fill the grid with one element's cells.
const OUTLINE_MAX_CELLS: usize = 1024;

/// A grid over the `building=*` outlines, for asking which outline covers a point.
///
/// `BTreeMap`, not a hash map, and every bucket is in insertion order, which is OSM id
/// order — so the answer to "which outline covers this part" does not depend on
/// iteration order (crate rule 2).
struct OutlineIndex {
    cells: BTreeMap<(i64, i64), Vec<usize>>,
    oversized: Vec<usize>,
}

impl OutlineIndex {
    /// Which cell a point falls in.
    fn cell(p: Vec3) -> (i64, i64) {
        (
            (p.x / OUTLINE_CELL_M).floor() as i64,
            (p.y / OUTLINE_CELL_M).floor() as i64,
        )
    }

    /// Builds the index over `rings`, which are the closed outer rings of the outlines.
    fn build<'a>(rings: impl Iterator<Item = (usize, &'a [Vec3])>) -> Self {
        let mut index = OutlineIndex {
            cells: BTreeMap::new(),
            oversized: Vec::new(),
        };
        for (which, ring) in rings {
            let mut min = Self::cell(ring[0]);
            let mut max = min;
            for p in ring {
                let c = Self::cell(*p);
                min = (min.0.min(c.0), min.1.min(c.1));
                max = (max.0.max(c.0), max.1.max(c.1));
            }
            let span = ((max.0 - min.0 + 1) as usize).saturating_mul((max.1 - min.1 + 1) as usize);
            if span > OUTLINE_MAX_CELLS {
                index.oversized.push(which);
                continue;
            }
            for cx in min.0..=max.0 {
                for cy in min.1..=max.1 {
                    index.cells.entry((cx, cy)).or_default().push(which);
                }
            }
        }
        index
    }

    /// The outlines whose cells the box `min..=max` touches, plus the oversized ones, in
    /// ascending candidate index and without repeats.
    ///
    /// A part is a building-sized polygon, so this is a handful of 100 m cells and the
    /// scan stays proportional to the parts, not to the parts times the outlines. The
    /// result is sorted, so it is ordered by candidate index rather than by which cell
    /// happened to list an outline first (crate rule 2): the caller's tie-break is then
    /// meaningful. An outline whose cells are not in the list cannot share area with the
    /// box, so nothing is lost by not visiting it.
    fn candidates_over(&self, min: Vec3, max: Vec3, out: &mut Vec<usize>) {
        out.clear();
        out.extend(self.oversized.iter().copied());
        let (lo, hi) = (Self::cell(min), Self::cell(max));
        let span = ((hi.0 - lo.0 + 1) as usize).saturating_mul((hi.1 - lo.1 + 1) as usize);
        if span > OUTLINE_MAX_CELLS {
            // A "part" spanning 10 km is a mistagged relation, not a building. Rather
            // than walk a million empty cells for it, offer it every indexed outline;
            // the caller's own box test throws out the ones that cannot overlap.
            out.extend(self.cells.values().flatten().copied());
        } else {
            for cx in lo.0..=hi.0 {
                for cy in lo.1..=hi.1 {
                    if let Some(cell) = self.cells.get(&(cx, cy)) {
                        out.extend(cell.iter().copied());
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// One candidate's `building=*` outline: its closed outer ring and the bounding box of
/// that ring.
///
/// There is one of these per candidate, empty for every candidate that is not an
/// outline, so a grid bucket's index is a direct index into the slice. The box is
/// computed once here rather than per part, because the part scan asks for it 76 000
/// times on the Phase 1 extract and the rings do not move.
struct Outline {
    /// The closed outer ring, or empty when this candidate is not an outline.
    ring: Vec<Vec3>,
    /// The ring's bounding box, or `None` when there is no usable ring.
    bounds: Option<(Vec3, Vec3)>,
}

impl Outline {
    /// Wraps a ring, which may be empty, and measures it.
    fn new(ring: Vec<Vec3>) -> Self {
        let bounds = if ring.len() >= 4 {
            ring_bounds(&ring)
        } else {
            None
        };
        Outline { ring, bounds }
    }
}

/// One building-shaped polygon, resolved to points and ready to be judged.
struct Candidate<'a> {
    /// The OSM way or relation id it came from.
    element: i64,
    /// What it is.
    role: BuildingRole,
    /// Its outer ring, open (the first point is not repeated).
    ring: Vec<Vec3>,
    /// Its interior holes.
    holes: Vec<Vec<Vec3>>,
    /// Its tags.
    tags: &'a Tags,
}

/// Imports every building: closed `building` ways, plus `type=multipolygon` relations with
/// their inner rings kept as holes.
///
/// A way that is a member of a building multipolygon is imported as part of that
/// multipolygon and not again on its own. Ids are assigned ways first in OSM id order, then
/// relations in OSM id order.
///
/// # One structure, one obstacle (V1)
///
/// OSM *Simple 3D Buildings* splits a complex structure into a `building=*` **outline**
/// and the `building:part=*` **volumes** inside it. The outline is the structure; the
/// parts are its detail. So:
///
/// * an outline always becomes a building;
/// * a part that shares its footprint with an outline is **folded into it** — which
///   outline is [`choose_parent`]'s question, and it is answered by clipped overlap area
///   rather than by a sampled point. The outline already covers that ground, and counting
///   the part again would give the same wall two or three attenuations in any obstacle
///   model that sums over intersected buildings. Folding means the part's height goes into the outline: the
///   outline's height becomes the taller of its own and the tallest part inside it, with
///   [`HeightSource::FromParts`] when the parts won. It is counted in
///   [`ImportCounts::building_parts_merged`];
/// * **a part with no outline over it is kept as a building.** This does occur, and it is
///   not a mapping error to ignore: a mapper who tags only parts leaves a real structure
///   that would otherwise vanish from the world entirely. It is counted in
///   [`ImportCounts::building_parts_orphan`] and raised as
///   [`Anomaly::BuildingPartWithoutOutline`], so the share is visible rather than
///   assumed.
///
/// # What the fold loses, and why it is a simplification rather than a fix
///
/// The parts' own **geometry** is not kept: [`Building`] has no field for a sub-volume,
/// the `vwp-v1` building record on the wire has no `min_height` column to put one in
/// (§4.4), and nothing downstream reads one yet. So a stepped tower is a single prism as
/// tall as its tallest part, not a stack of volumes, and an obstacle model sees more
/// mass at high altitude than the structure really has — the setback above a podium is
/// filled in. That is a deliberate simplification, it is on the model card as
/// `building_parts_as_max_height`, and reversing it means adding a parts list to
/// [`Building`], a `min_height_m` column to the wire format and a stacked-prism obstacle
/// model to go with them.
///
/// It is the right way round for a propagation study all the same: the old behaviour
/// dropped the part entirely and left a 10 m box where a 279 m tower stands.
#[allow(clippy::too_many_lines)]
fn build_buildings(
    file: &OsmFile,
    points: &[Vec3],
    keep: &RingFilter<'_>,
    options: &OsmOptions,
    symbols: &mut SymbolTable,
    report: &mut ImportReport,
) -> Vec<Building> {
    // Ways consumed by a building multipolygon.
    //
    // A member way of a building multipolygon is normally a bare ring: its tags, if any,
    // belong to the relation. One case is not, and it cost this world the Empire State
    // Building. A `building:part` multipolygon may borrow the `building=*` outline way
    // as its own outer ring — relation 10872054 borrows way 34633854, which carries the
    // name, `building=office` and `height=443.2` — and consuming that way deletes the
    // structure. A part may not swallow an outline: it subdivides one.
    let mut consumed: BTreeSet<i64> = BTreeSet::new();
    for relation in &file.relations {
        if !(relation.tags.is("type", "multipolygon") && is_building_polygon(&relation.tags)) {
            continue;
        }
        let part_relation = building_role(&relation.tags) == Some(BuildingRole::Part);
        for member in &relation.members {
            if member.kind != MemberKind::Way {
                continue;
            }
            let member_is_outline = file
                .way(member.id)
                .is_some_and(|w| building_role(&w.tags) == Some(BuildingRole::Outline));
            if part_relation && member_is_outline {
                report.note(Anomaly::OutlineInPartRelation, member.id);
                continue;
            }
            consumed.insert(member.id);
        }
    }

    // --- pass 1: every candidate, in id order -----------------------------------
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for way in &file.ways {
        if consumed.contains(&way.id) {
            continue;
        }
        let Some(role) = building_role(&way.tags) else {
            continue;
        };
        let Some(ring) = resolve_ring(&way.nodes, file, points) else {
            report.note(Anomaly::RingTooShort, way.id);
            continue;
        };
        let Some((kept, cut)) = keep(&ring) else {
            report.note(Anomaly::ClippedOut, way.id);
            continue;
        };
        if cut {
            report.counts.polygons_clipped += 1;
        }
        candidates.push(Candidate {
            element: way.id,
            role,
            ring: kept,
            holes: Vec::new(),
            tags: &way.tags,
        });
    }
    for relation in &file.relations {
        if !relation.tags.is("type", "multipolygon") {
            continue;
        }
        let Some(role) = building_role(&relation.tags) else {
            continue;
        };
        let (outers, inners) = multipolygon_rings(relation, file, points, report);
        if outers.is_empty() {
            report.note(Anomaly::NoOuterRing, relation.id);
            continue;
        }
        // One building per outer ring; every inner ring is offered to the outer ring that
        // contains its first point, which is the only assignment a flat model can make.
        for outer in outers {
            let Some((kept, cut)) = keep(&outer) else {
                report.note(Anomaly::ClippedOut, relation.id);
                continue;
            };
            if cut {
                report.counts.polygons_clipped += 1;
            }
            let closed_outer = closed(&kept);
            let holes: Vec<Vec<Vec3>> = inners
                .iter()
                .filter(|inner| crate::model::point_in_ring(&closed_outer, inner[0]))
                .filter_map(|inner| keep(inner).map(|(ring, _)| ring))
                .collect();
            candidates.push(Candidate {
                element: relation.id,
                role,
                ring: kept,
                holes,
                tags: &relation.tags,
            });
        }
    }

    // --- pass 2: the outline index ------------------------------------------------
    // One entry per candidate, empty for everything that is not an outline, so a grid
    // bucket's index is a direct index into it.
    let outlines: Vec<Outline> = candidates
        .iter()
        .map(|c| {
            let ring = if c.role == BuildingRole::Outline {
                closed(&c.ring)
            } else {
                Vec::new()
            };
            Outline::new(ring)
        })
        .collect();
    let index = OutlineIndex::build(
        outlines
            .iter()
            .enumerate()
            .filter(|(_, o)| o.bounds.is_some())
            .map(|(i, o)| (i, o.ring.as_slice())),
    );

    // --- pass 3: fold every part's height into the outline that covers it ----------
    // This is what "folded" has to mean. Dropping the part and leaving the outline to
    // its own tags threw away the height: on the Phase 1 extract 4 323 tagged heights
    // went with the parts, the Chrysler Building came out 10 m tall, and half of
    // Midtown's built volume vanished. Midtown is mapped part-first — the Chrysler's
    // outline carries no `height` at all — so the parts are where the heights live.
    //
    // The outline's obstacle height is therefore the taller of its own height and the
    // top of the tallest part inside it, and [`HeightSource::FromParts`] says when the
    // parts won. A part is measured with the same [`decide_height`] as a building, so
    // the spire cap applies to it before it is folded: the Empire State Building's
    // 443.2 m mast folds in as its 330 m structural top, not as a 443 m prism.
    let mut part_top_m = vec![0.0f64; candidates.len()];
    let mut folded = vec![false; candidates.len()];

    // 3a: the outlines that claim each part, by real footprint overlap, and the parts
    // exactly one outline claims. Those are assignable on the spot, and they are also
    // what marks an outline as **subdivided** — a structure whose ground is mapped part
    // by part — which is the third rung of [`choose_parent`]'s ladder. Only the
    // unambiguous parts mark it, so the mark does not depend on the order the contested
    // ones are settled in (crate rule 2).
    let mut owner_of: Vec<Option<usize>> = vec![None; candidates.len()];
    let mut scratch: Vec<usize> = Vec::new();
    let mut subdivided = vec![false; candidates.len()];
    let mut contested: Vec<(usize, Vec<(f64, usize)>)> = Vec::new();
    for i in 0..candidates.len() {
        if candidates[i].role != BuildingRole::Part {
            continue;
        }
        let claims = outline_claims(&candidates[i].ring, &index, &outlines, &mut scratch);
        match claims.len() {
            0 => {}
            1 => {
                owner_of[i] = Some(claims[0].1);
                subdivided[claims[0].1] = true;
            }
            _ => contested.push((i, claims)),
        }
    }

    // 3b: the parts more than one outline claims — nine of 4 373 on the Phase 1 extract.
    for (i, claims) in &contested {
        owner_of[*i] = Some(choose_parent(
            ring_area_m2(&candidates[*i].ring),
            claims,
            &outlines,
            &subdivided,
        ));
    }

    // 3c: fold each part's height into the outline that owns it.
    for i in 0..candidates.len() {
        let Some(owner) = owner_of[i] else {
            continue;
        };
        let (element, tags) = (candidates[i].element, candidates[i].tags);
        let decision = decide_height(tags, element, options, report);
        if decision.spire_capped {
            report.note(Anomaly::SpireHeightCapped, element);
            report.counts.buildings_spire_capped += 1;
        }
        folded[i] = true;
        part_top_m[owner] = part_top_m[owner].max(decision.height_m);
    }

    // --- pass 4: emit -------------------------------------------------------------
    let mut out: Vec<Building> = Vec::new();
    for (i, candidate) in candidates.iter().enumerate() {
        match candidate.role {
            BuildingRole::Subsurface => {
                report.note(Anomaly::SubsurfaceStructure, candidate.element);
                report.counts.buildings_subsurface += 1;
                continue;
            }
            BuildingRole::Part => {
                if folded[i] {
                    report.counts.building_parts_merged += 1;
                    continue;
                }
                report.note(Anomaly::BuildingPartWithoutOutline, candidate.element);
                report.counts.building_parts_orphan += 1;
            }
            BuildingRole::Outline => {}
        }
        let mut decision = decide_height(candidate.tags, candidate.element, options, report);
        // The fold. `part_top_m` is zero for everything that is not an outline, so this
        // is a no-op for an orphan part.
        if part_top_m[i] > decision.height_m {
            decision.height_m = part_top_m[i];
            decision.source = HeightSource::FromParts;
        }
        let id = BuildingId::new(out.len() as u32);
        let holes = if options.import.keep_building_holes {
            candidate.holes.clone()
        } else {
            Vec::new()
        };
        match Building::new(
            id,
            candidate.ring.clone(),
            holes,
            decision.height_m,
            decision.min_height_m,
            material_of(candidate.tags),
            decision.source,
        ) {
            Ok(mut building) => {
                building.levels = decision.levels;
                building.name = candidate
                    .tags
                    .get("name")
                    .and_then(|n| symbols.intern_optional(n));
                match decision.source {
                    HeightSource::Tagged => report.counts.heights_tagged += 1,
                    HeightSource::FromLevels => report.counts.heights_from_levels += 1,
                    HeightSource::FromParts => report.counts.heights_from_parts += 1,
                    HeightSource::Defaulted => report.counts.heights_defaulted += 1,
                }
                if decision.spire_capped {
                    report.note(Anomaly::SpireHeightCapped, candidate.element);
                    report.counts.buildings_spire_capped += 1;
                }
                report.counts.building_holes += building.holes.len() as u64;
                out.push(building);
            }
            Err(_) => report.note(Anomaly::RingTooShort, candidate.element),
        }
    }
    out
}

/// The least share of a part's own footprint that an outline must cover before it counts
/// as a claim on that part.
///
/// A part is a *subdivision* of the structure it belongs to, so the structure's outline
/// covers essentially all of it: on the Phase 1 extract every one of the 4 368 folded
/// parts is at least 0.934 covered by the outline it is folded into, and 4 364 of them
/// are covered 0.983 or more. A half is therefore a threshold with nothing near it in
/// either direction — it says "more of this part is inside that outline than is outside
/// it" — and moving it anywhere between 0.35 and 0.93 changes no part's parent on the
/// real extract.
///
/// It exists at all because *some* floor is needed: without one, a part that merely
/// grazes a neighbour it does not belong to — two footprints drawn from different
/// surveys overlap by a metre along a shared wall — would be folded into that neighbour
/// and lend it its full height, which is the same defect this rule is here to remove.
const PART_COVERAGE_MIN: f64 = 0.5;

/// The share of a part's footprint an outline must cover before the part counts as
/// lying *wholly inside* it rather than merely mostly inside it.
///
/// A part that subdivides an outline is drawn inside it, very often on its own nodes, so
/// the honest reading of "inside" is 1.0 and this is 1.0 less the slack that the
/// projection and the mapper's drawing leave. The extract says the same: of the nine
/// parts more than one outline claims, the covering fraction is either 1.0000 to four
/// decimals or 0.9553 or less, so anywhere from 0.96 to 1.0 selects the same set. The
/// distinction matters because the tie-breaks below only make sense between outlines
/// that each contain the *whole* part; a part that hangs over the edge of one candidate
/// and not the other is already decided by area.
const PART_INSIDE_MIN: f64 = 0.99;

/// The share of an outline that a second outline must cover before the first counts as
/// nested inside the second.
///
/// Same reasoning and same slack as [`PART_INSIDE_MIN`], one level up: the United
/// Nations Secretariat Building's outline is 1.0000 inside the United Nations
/// Headquarters outline, and the next-closest pair among the contested parts is 0.9906,
/// which is [`is_nested_in`]'s only near miss and is not a nesting — it is a market hall
/// that pokes out of the Helmsley Building's footprint by 15 m².
const OUTLINE_NESTED_MIN: f64 = 0.999;

/// Every indexed `building=*` outline that claims `ring`: the ones sharing at least
/// [`PART_COVERAGE_MIN`] of the part's own footprint, as `(shared m², candidate index)`
/// in ascending candidate index.
///
/// Area, not a sampled point. The question "does this part sit inside that structure" is
/// a question about two polygons, and any single probe point answers a different one.
///
/// A probe point was the rule until it put four Midtown structures on the wrong
/// footprint. The failure is not exotic. A part that overhangs its own outline — or one
/// whose vertex mean falls outside its own concave ring — has a probe point that can
/// land in a *neighbouring* outline, and the old rule then picked the **smallest**
/// outline containing that one point. Part 1473999184 is 24 m² of Rose Hill (905 m²,
/// 195 m); its mean landed in the 156 m² building next door, whose own height is 25 m,
/// so a 155 m prism was imported on a 156 m² footprint and Rose Hill lost its height.
/// Parts 292032000, 292032001 and 292032005 were folded into outlines they do not touch
/// at all.
///
/// The spatial pre-filter survives: the grid answers in whole 100 m cells, an exact
/// bounding-box test throws out most of what a cell offers, and only what is left is
/// clipped. The scan stays proportional to the parts, not to the parts times the
/// outlines.
fn outline_claims(
    ring: &[Vec3],
    index: &OutlineIndex,
    outlines: &[Outline],
    scratch: &mut Vec<usize>,
) -> Vec<(f64, usize)> {
    let Some((part_min, part_max)) = ring_bounds(ring) else {
        return Vec::new();
    };
    // A part with no area at all — a ring folded onto a line — shares nothing with
    // anything, so the floor below leaves it an orphan.
    let floor = PART_COVERAGE_MIN * ring_area_m2(ring);
    let mut claims: Vec<(f64, usize)> = Vec::new();
    index.candidates_over(part_min, part_max, scratch);
    for which in scratch.iter().copied() {
        let Some((min, max)) = outlines[which].bounds else {
            continue;
        };
        // The index answers in whole 100 m cells; this is the exact box test, and on the
        // Phase 1 extract it rejects 86% of what the cells offer before any clipping.
        if min.x > part_max.x || max.x < part_min.x || min.y > part_max.y || max.y < part_min.y {
            continue;
        }
        let shared = polygon_overlap_m2(ring, &outlines[which].ring);
        if shared > 0.0 && shared >= floor {
            claims.push((shared, which));
        }
    }
    claims
}

/// True if outline `inner` lies inside outline `outer`, up to [`OUTLINE_NESTED_MIN`].
fn is_nested_in(inner: &[Vec3], outer: &[Vec3]) -> bool {
    let area = ring_area_m2(inner);
    area > 0.0 && polygon_overlap_m2(inner, outer) >= OUTLINE_NESTED_MIN * area
}

/// Which of the outlines that claim a part is its parent. `claims` is what
/// [`outline_claims`] returned and holds at least two entries.
///
/// # The ladder, and the case that forces each rung
///
/// 1. **The outline that shares the most footprint with the part.** Nearly every
///    contested part is settled here: part 261243304 is 1.0000 inside One United Nations
///    Plaza and 0.8533 inside the roof next door, and part 291189626 is 1.0000 inside
///    the Helmsley Building and 0.9553 inside the market hall in its concourse. A part
///    that hangs over the edge of a candidate is not a subdivision of it.
///
/// 2. **Where several outlines contain the whole part ([`PART_INSIDE_MIN`]), the
///    innermost one** — the candidate nested inside all the others. The United Nations
///    Secretariat Building's outline (1 920 m²) is drawn wholly inside the United
///    Nations Headquarters outline (16 441 m², the whole campus), and the Secretariat's
///    two parts are 1.0000 inside both. They are the Secretariat's: the campus outline
///    is the site, the Secretariat outline is the building on it, and taking the campus
///    would stand a 156 m prism on 16 441 m² of lawn and river frontage. Nesting decides
///    this one and area cannot — area would pick the campus, and the smallest-area rule
///    that this replaces happened to pick the Secretariat for the same reason it picked
///    the wrong parent elsewhere.
///
/// 3. **Otherwise, the outline that is already subdivided into parts.** Two named cases
///    reach this rung and nothing else in the extract does. Part 1473999184 (24 m²,
///    155 m, starting 60 m up) is 1.0000 inside both Rose Hill (905 m²) and the 156 m²
///    building beside it, because those two footprints overlap in a 23.8 m² sliver and
///    the part *is* that sliver: it is the tower's overhang over its neighbour's lot,
///    drawn on the neighbour's own nodes. Neither outline is nested in the other — they
///    share 2.6% and 15.2% of themselves — so rung 2 says nothing. What separates them
///    is that Rose Hill is mapped part by part (seven parts of its own, no other
///    claimant) and the neighbour is a plain 25.2 m footprint with no parts at all: a
///    structure that is not subdivided has no subdivisions, so the sliver is Rose Hill's.
///    Part 283472143 is the same shape of case at 785 Eighth Avenue (four parts of its
///    own) against a three-storey neighbour with none.
///
/// 4. **Ascending candidate index**, which is ascending OSM way id and then ascending
///    OSM relation id — the order pass 1 builds candidates in. Nothing in the extract
///    reaches this rung; it is here so that the answer can never depend on iteration
///    order (crate rule 2), and [`OutlineIndex::candidates_over`] hands the claims over
///    in that order for the same reason.
///
/// # What it does not look at
///
/// Interior holes. [`Outline`] carries outer rings only, so a part standing in a
/// courtyard counts as inside the outline around the courtyard, exactly as it did under
/// the point test. Folding it in is also the conservative answer for an obstacle model:
/// it raises an existing structure rather than inventing a free-standing one.
///
/// Tags, too — heights especially. The wrong parents all had a tagged height far below
/// the part's, so "a part may not be taller than its parent says it is" would also have
/// caught them, but it would be wrong in general: Midtown is mapped part-first and the
/// Chrysler Building's outline carries no height at all, which is exactly why the fold
/// exists.
fn choose_parent(
    part_area: f64,
    claims: &[(f64, usize)],
    outlines: &[Outline],
    subdivided: &[bool],
) -> usize {
    // Rung 1.
    let mut best = claims[0];
    for &(shared, which) in &claims[1..] {
        if shared > best.0 {
            best = (shared, which);
        }
    }
    let inside: Vec<usize> = claims
        .iter()
        .filter(|(shared, _)| *shared >= PART_INSIDE_MIN * part_area)
        .map(|(_, which)| *which)
        .collect();
    if inside.len() < 2 {
        return best.1;
    }
    // Rung 2: the candidate nested inside every other candidate.
    let mut innermost: Option<usize> = None;
    for &which in &inside {
        let nested_in_all = inside.iter().all(|&other| {
            other == which || is_nested_in(&outlines[which].ring, &outlines[other].ring)
        });
        if nested_in_all {
            // Two coincident outlines are each nested in the other; the lower index
            // wins, which is rung 4 reached early.
            innermost = Some(innermost.map_or(which, |prev: usize| prev.min(which)));
        }
    }
    if let Some(which) = innermost {
        return which;
    }
    // Rung 3, then rung 4. `inside` is in ascending candidate index, so `find` is the
    // lowest index in whichever set it is asked for.
    inside
        .iter()
        .find(|&&which| subdivided[which])
        .or_else(|| inside.first())
        .copied()
        .unwrap_or(best.1)
}

/// The axis-aligned bounds of a ring as `(min, max)`, or `None` if it has no points.
fn ring_bounds(ring: &[Vec3]) -> Option<(Vec3, Vec3)> {
    let first = *ring.first()?;
    let (mut min, mut max) = (first, first);
    for p in ring {
        min = Vec3::new_2d(min.x.min(p.x), min.y.min(p.y));
        max = Vec3::new_2d(max.x.max(p.x), max.y.max(p.y));
    }
    Some((min, max))
}

/// A ring without the closing point, so that a closed ring and an open one describe the
/// same polygon to the area code below.
fn open_slice(ring: &[Vec3]) -> &[Vec3] {
    match (ring.first(), ring.last()) {
        (Some(f), Some(l)) if ring.len() > 1 && f.x == l.x && f.y == l.y => &ring[..ring.len() - 1],
        _ => ring,
    }
}

/// The area two simple polygons share, m². Zero when they are disjoint or touch only
/// along an edge.
///
/// Each polygon is decomposed into the fan of triangles `(o, v[i], v[i+1])` about a
/// common origin `o`, taken **signed**: the sum of the signed fans is the polygon's own
/// signed area, and the sum of the signed indicator functions is the polygon's indicator
/// function, including where the fan folds back over itself. A concave footprint is
/// therefore exact, not approximated, and no polygon-clipping library is needed — the
/// only clip performed is triangle against triangle, and a triangle is convex, so
/// Sutherland–Hodgman is exact for it.
///
/// So the shared area is `Σ_i Σ_j sign(a_i)·sign(b_j)·|a_i ∩ b_j|` over the two fans,
/// with both rings first turned counter-clockwise so that the sum comes out positive.
///
/// Arithmetic: multiply, add, subtract, and one division per clipped edge. No
/// `sqrt`, no trigonometry, nothing from the platform's libm (crate rule 4), so two
/// machines agree on the last bit. The summation order is `i` then `j` over the rings as
/// given, so it is also the same number on every run.
///
/// Cost is `O(n·m)` triangle clips for an `n`- and an `m`-vertex ring, which is why the
/// caller reaches it only for a candidate whose bounding box already meets the part's.
fn polygon_overlap_m2(a: &[Vec3], b: &[Vec3]) -> f64 {
    let (a, b) = (open_slice(a), open_slice(b));
    if a.len() < 3 || b.len() < 3 {
        return 0.0;
    }
    // Counter-clockwise, and about an origin on the smaller ring: the coordinates that
    // reach the cross products are then metres across one building rather than
    // kilometres across the city, which is where the precision goes.
    let ccw_a = crate::model::ring_signed_area_2x(a) >= 0.0;
    let ccw_b = crate::model::ring_signed_area_2x(b) >= 0.0;
    let at = |i: usize| a[if ccw_a { i } else { a.len() - 1 - i }];
    let bt = |j: usize| b[if ccw_b { j } else { b.len() - 1 - j }];
    let origin = a[0];
    let mut acc = 0.0;
    for i in 0..a.len() {
        let (a0, a1) = (at(i) - origin, at((i + 1) % a.len()) - origin);
        let sa = a0.x * a1.y - a1.x * a0.y;
        if sa == 0.0 {
            continue;
        }
        let tri_a = if sa > 0.0 {
            [Vec3::ZERO, a0, a1]
        } else {
            [Vec3::ZERO, a1, a0]
        };
        for j in 0..b.len() {
            let (b0, b1) = (bt(j) - origin, bt((j + 1) % b.len()) - origin);
            let sb = b0.x * b1.y - b1.x * b0.y;
            if sb == 0.0 {
                continue;
            }
            let tri_b = if sb > 0.0 {
                [Vec3::ZERO, b0, b1]
            } else {
                [Vec3::ZERO, b1, b0]
            };
            let shared = triangle_overlap_m2(tri_a, tri_b);
            if shared == 0.0 {
                continue;
            }
            if (sa > 0.0) == (sb > 0.0) {
                acc += shared;
            } else {
                acc -= shared;
            }
        }
    }
    // Both fans are of the same handedness as their ring, so the sum is non-negative up
    // to rounding; a hair below zero is a cancelled sum, not a negative area.
    acc.max(0.0)
}

/// The area two counter-clockwise triangles share, m².
///
/// Sutherland–Hodgman: the first triangle is cut by each edge of the second in turn.
/// Clipping a convex polygon by a half-plane adds at most one vertex, so three cuts of a
/// triangle can reach six vertices and never more — the buffers are fixed-size and
/// nothing allocates.
fn triangle_overlap_m2(subject: [Vec3; 3], clip: [Vec3; 3]) -> f64 {
    let mut poly = [Vec3::ZERO; 8];
    let mut n = 3;
    poly[..3].copy_from_slice(&subject);
    let mut next = [Vec3::ZERO; 8];
    for e in 0..3 {
        let (c0, c1) = (clip[e], clip[(e + 1) % 3]);
        let (ex, ey) = (c1.x - c0.x, c1.y - c0.y);
        // Positive to the left of the directed edge, which is inside a CCW triangle.
        let side = |p: Vec3| ex * (p.y - c0.y) - ey * (p.x - c0.x);
        let mut m = 0;
        for k in 0..n {
            let (p, q) = (poly[k], poly[(k + 1) % n]);
            let (dp, dq) = (side(p), side(q));
            if dp >= 0.0 {
                next[m] = p;
                m += 1;
            }
            if (dp > 0.0 && dq < 0.0) || (dp < 0.0 && dq > 0.0) {
                let t = dp / (dp - dq);
                next[m] = Vec3::new_2d(p.x + t * (q.x - p.x), p.y + t * (q.y - p.y));
                m += 1;
            }
        }
        n = m;
        poly[..n].copy_from_slice(&next[..n]);
        if n < 3 {
            return 0.0;
        }
    }
    // Shoelace over the clipped convex polygon, in the order it was built.
    let mut twice = 0.0;
    for k in 0..n {
        let (p, q) = (poly[k], poly[(k + 1) % n]);
        twice += p.x * q.y - q.x * p.y;
    }
    (twice * 0.5).abs()
}

/// The area of a closed ring, m².
///
/// Multiplications and additions only (crate rule 1: no std transcendental), so it is
/// the same number on every platform.
fn ring_area_m2(ring: &[Vec3]) -> f64 {
    crate::model::ring_signed_area_2x(ring).abs() * 0.5
}

/// A ring with its first point repeated at the end, for the point-in-polygon test.
fn closed(ring: &[Vec3]) -> Vec<Vec3> {
    let mut out = ring.to_vec();
    if out.first() != out.last() {
        if let Some(first) = out.first().copied() {
            out.push(first);
        }
    }
    out
}

/// The outer and inner rings of one multipolygon relation, as projected points.
fn multipolygon_rings(
    relation: &RawRelation,
    file: &OsmFile,
    points: &[Vec3],
    report: &mut ImportReport,
) -> (Vec<Vec<Vec3>>, Vec<Vec<Vec3>>) {
    let mut outer_pool: Vec<(i64, Vec<i64>)> = Vec::new();
    let mut inner_pool: Vec<(i64, Vec<i64>)> = Vec::new();
    for member in &relation.members {
        if member.kind != MemberKind::Way {
            continue;
        }
        let Some(way) = file.way(member.id) else {
            report.note(Anomaly::MissingNode, relation.id);
            continue;
        };
        let entry = (way.id, way.nodes.clone());
        match member.role.as_str() {
            "inner" => inner_pool.push(entry),
            _ => outer_pool.push(entry),
        }
    }
    let to_points = |rings: Vec<Vec<i64>>| -> Vec<Vec<Vec3>> {
        rings
            .into_iter()
            .filter_map(|ring| resolve_ring(&ring, file, points))
            .collect()
    };
    (
        to_points(chain_rings(outer_pool, relation.id, report)),
        to_points(chain_rings(inner_pool, relation.id, report)),
    )
}

/// The land-use class a tag set maps to, or `None` if it is not a land-use area.
fn landuse_class(tags: &Tags) -> Option<LanduseClass> {
    if let Some(v) = tags.get("landuse") {
        return Some(match v {
            "residential" => LanduseClass::Suburban,
            "commercial" | "retail" | "education" | "institutional" => LanduseClass::Urban,
            "industrial" | "construction" | "quarry" | "railway" | "port" | "depot" => {
                LanduseClass::Industrial
            }
            "farmland" | "farmyard" | "orchard" | "vineyard" | "allotments" | "greenfield" => {
                LanduseClass::Rural
            }
            "forest" | "meadow" | "grass" | "village_green" | "recreation_ground" | "cemetery" => {
                LanduseClass::Park
            }
            "reservoir" | "basin" | "salt_pond" => LanduseClass::Water,
            _ => return None,
        });
    }
    if let Some(v) = tags.get("natural") {
        return Some(match v {
            "wood" | "scrub" | "grassland" | "heath" => LanduseClass::Park,
            "water" | "bay" | "strait" | "wetland" => LanduseClass::Water,
            _ => return None,
        });
    }
    if let Some(v) = tags.get("leisure") {
        return Some(match v {
            "park" | "garden" | "golf_course" | "pitch" | "playground" | "nature_reserve" => {
                LanduseClass::Park
            }
            _ => return None,
        });
    }
    None
}

/// Imports land-use zones: closed `landuse`, `natural` and `leisure` ways and
/// multipolygons.
fn build_landuse(
    file: &OsmFile,
    points: &[Vec3],
    keep: &RingFilter<'_>,
    symbols: &mut SymbolTable,
    report: &mut ImportReport,
) -> Vec<LanduseZone> {
    let mut consumed: BTreeSet<i64> = BTreeSet::new();
    for relation in &file.relations {
        if relation.tags.is("type", "multipolygon") && landuse_class(&relation.tags).is_some() {
            for member in &relation.members {
                if member.kind == MemberKind::Way {
                    consumed.insert(member.id);
                }
            }
        }
    }
    let mut out: Vec<LanduseZone> = Vec::new();
    let mut push = |ring: Vec<Vec3>,
                    class: LanduseClass,
                    tags: &Tags,
                    element: i64,
                    report: &mut ImportReport,
                    out: &mut Vec<LanduseZone>| {
        let Some((kept, cut)) = keep(&ring) else {
            return;
        };
        if cut {
            report.counts.polygons_clipped += 1;
        }
        let id = ZoneId::new(out.len() as u32);
        match LanduseZone::new(id, kept, class, class.default_env()) {
            Ok(mut zone) => {
                zone.name = tags.get("name").and_then(|n| symbols.intern_optional(n));
                out.push(zone);
            }
            Err(_) => report.note(Anomaly::RingTooShort, element),
        }
    };
    for way in &file.ways {
        if consumed.contains(&way.id) || is_building_polygon(&way.tags) {
            continue;
        }
        let Some(class) = landuse_class(&way.tags) else {
            continue;
        };
        if way.nodes.first() != way.nodes.last() {
            continue; // an open way is a boundary line, not an area
        }
        let Some(ring) = resolve_ring(&way.nodes, file, points) else {
            report.note(Anomaly::RingTooShort, way.id);
            continue;
        };
        push(ring, class, &way.tags, way.id, report, &mut out);
    }
    for relation in &file.relations {
        if !relation.tags.is("type", "multipolygon") {
            continue;
        }
        let Some(class) = landuse_class(&relation.tags) else {
            continue;
        };
        let (outers, _) = multipolygon_rings(relation, file, points, report);
        for outer in outers {
            push(outer, class, &relation.tags, relation.id, report, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 3 and 10: projection, assembly, provenance
// ---------------------------------------------------------------------------

/// True if any of the way's nodes lies inside `bbox`.
fn way_in_bbox(way: &RawWay, file: &OsmFile, bbox: &GeoBbox) -> bool {
    way.nodes.iter().filter_map(|n| file.node(*n)).any(|n| {
        n.lat >= bbox.min_lat_deg
            && n.lat <= bbox.max_lat_deg
            && n.lon >= bbox.min_lon_deg
            && n.lon <= bbox.max_lon_deg
    })
}

/// The largest on-grid value not greater than `v`, on the `quantum` grid.
///
/// The world origin is the south-west corner of its own geometry (D6), so it must be
/// rounded **down**: rounding to nearest could put it a centimetre north-east of the
/// corner and give the first building a negative coordinate.
fn floor_to_grid(v: f64, quantum: f64) -> f64 {
    let rounded = quantise(v, quantum);
    if rounded > v {
        quantise(v - quantum, quantum)
    } else {
        rounded
    }
}

/// Imports an OSM XML file from disk.
///
/// Returns the world and the report of everything the importer found wrong with the file
/// or had to guess. The file's SHA-256 becomes the provenance's `source_id`, so a world
/// can always be traced back to the exact bytes it came from.
///
/// ```no_run
/// use v2xw_world::osm::{HighwayPreset, OsmOptions, import_osm};
/// // The speed preset is required: there is no default, because a default speed limit
/// // is a statement about a jurisdiction (V4/W1).
/// let options = OsmOptions::default()
///     .imported_at("2026-09-18T00:00:00Z")
///     .highway_preset(HighwayPreset::UrbanUsNyc);
/// let (world, report) = import_osm("worlds/cache/city.osm.xml", &options)?;
/// println!("{}", report.to_text());
/// assert!(world.counts().lanes > 0);
/// # Ok::<(), v2xw_world::WorldError>(())
/// ```
///
/// # Errors
///
/// [`WorldError::Io`] if the file cannot be read, [`WorldError::Malformed`] if it is not
/// well-formed XML, [`WorldError::InvalidParameter`] for options that cannot be satisfied,
/// and whatever the world model rejects. Bad *data* inside well-formed XML is reported,
/// not raised.
pub fn import_osm(path: impl AsRef<Path>, options: &OsmOptions) -> Result<(World, ImportReport)> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    import_osm_bytes(&bytes, &path.display().to_string(), options)
}

/// Imports an OSM XML document already in memory.
///
/// `source_id` is what the report and the provenance call it — a path, a URL, or a test's
/// name. The content hash of the *source* is computed here, so the provenance is the same
/// whether the bytes came from a file or a download.
///
/// # Errors
///
/// As [`import_osm`], without the I/O case.
pub fn import_osm_bytes(
    xml: &[u8],
    source_id: &str,
    options: &OsmOptions,
) -> Result<(World, ImportReport)> {
    options.validate()?;
    let file = parse_osm(xml)?;
    let digest = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(xml);
        hasher.finalize()
    };
    let mut report = ImportReport {
        source_id: source_id.to_string(),
        source_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        source_bytes: xml.len() as u64,
        ..ImportReport::default()
    };
    let world = import_parsed(&file, options, &mut report)?;
    Ok((world, report))
}

/// Turns a parsed file into a world. The stages are the ones the module header lists.
#[allow(clippy::too_many_lines)]
fn import_parsed(file: &OsmFile, options: &OsmOptions, report: &mut ImportReport) -> Result<World> {
    report.counts.osm_nodes = file.nodes.len() as u64;
    report.counts.osm_ways = file.ways.len() as u64;
    report.counts.osm_relations = file.relations.len() as u64;
    report.skipped_simplifications = skipped_simplifications(&options.simplify);
    report.highway_preset = options.highway_preset;
    report.bbox_clip = options.bbox_clip;
    report.requested_bbox = options.bbox;
    report.min_useful_lane_m = options.min_useful_lane_m;

    // R8: a document with no nodes at all is a failed download, not an empty bounding
    // box. Importing it produced a valid, empty, content-addressed world with **zero**
    // anomalies, which then went into the world cache under its own hash — so a timed-out
    // Overpass query was indistinguishable from a legitimately empty request.
    if file.nodes.is_empty() {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: match &file.remark {
                Some(remark) => {
                    format!("the OSM document carries no nodes; the server said: {remark}")
                }
                None => {
                    "the OSM document carries no nodes, so there is nothing to import".to_string()
                }
            },
        });
    }

    // --- stage 3a: the world frame (V3) ------------------------------------------
    // The frame has to be fixed *before* anything is projected, and it must not depend on
    // which kept way happens to overhang the request furthest: that made the origin, and
    // therefore every metre coordinate and the content hash, move when the extract was
    // re-fetched at a slightly different radius. In descending order of reproducibility:
    // the requested box, the extract's declared bounds, the geometry itself.
    let (frame, frame_box) = match (options.bbox, file.bounds) {
        (Some(b), _) => (FrameRule::RequestedBbox, Some(b)),
        (None, Some(b)) => (FrameRule::ExtractBounds, Some(b)),
        (None, None) => (FrameRule::ImportedGeometry, None),
    };
    report.frame = frame;
    let origin_of = |b: GeoBbox| {
        GeoOrigin::new(
            floor_to_grid(b.min_lat_deg, Q_DEGREES),
            floor_to_grid(b.min_lon_deg, Q_DEGREES),
            0.0,
        )
    };
    let frame_origin = frame_box.map(origin_of);
    // A requested box is grown by the clip margin before anything is filtered, so the
    // margin band is not emptied by the filter before the clip can keep it.
    let margin_m = if options.bbox.is_some() {
        options.bbox_clip.margin_m()
    } else {
        0.0
    };
    let keep_box: Option<GeoBbox> = options.bbox.map(|b| {
        if margin_m <= 0.0 {
            return b;
        }
        let projection =
            Projection::new(frame_origin.expect("a requested box always fixes the frame"));
        let d_lat = margin_m / projection.metres_per_degree_latitude();
        let d_lon = margin_m / projection.metres_per_degree_longitude();
        GeoBbox::new(
            b.min_lat_deg - d_lat,
            b.min_lon_deg - d_lon,
            b.max_lat_deg + d_lat,
            b.max_lon_deg + d_lon,
        )
    });

    // --- stage 2: classify -------------------------------------------------------
    let mut symbols = SymbolTable::new();
    let mut plans: Vec<WayPlan> = Vec::new();
    for (index, way) in file.ways.iter().enumerate() {
        if let Some(bbox) = &keep_box {
            if way.tags.has("highway") && !way_in_bbox(way, file, bbox) {
                report.note(Anomaly::ClippedOut, way.id);
                continue;
            }
        }
        if let Some(plan) = classify_way(way, index, options, &mut symbols, report) {
            match plan.family {
                WayFamily::Motor => report.counts.drivable_ways += 1,
                WayFamily::Cycle => report.counts.cycle_ways += 1,
                WayFamily::Foot => report.counts.pedestrian_ways += 1,
            }
            plans.push(plan);
        }
    }

    // --- stage 3b: the local tangent plane ---------------------------------------
    // The scan over the geodetic coordinates themselves is what the frame falls back on
    // and what the provenance records as the realised source extent; the projection is
    // monotone in latitude and longitude, so the minimum of one is the minimum of the
    // other.
    let mut min_lat = f64::INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    let consider = |way: &RawWay,
                    min_lat: &mut f64,
                    min_lon: &mut f64,
                    max_lat: &mut f64,
                    max_lon: &mut f64| {
        for node in way.nodes.iter().filter_map(|n| file.node(*n)) {
            *min_lat = min_lat.min(node.lat);
            *min_lon = min_lon.min(node.lon);
            *max_lat = max_lat.max(node.lat);
            *max_lon = max_lon.max(node.lon);
        }
    };
    for plan in &plans {
        consider(
            &file.ways[plan.way],
            &mut min_lat,
            &mut min_lon,
            &mut max_lat,
            &mut max_lon,
        );
    }
    let area_wanted = options.layers.buildings || options.layers.landuse;
    for way in &file.ways {
        if !area_wanted {
            break;
        }
        let interesting = (options.layers.buildings && is_building_polygon(&way.tags))
            || (options.layers.landuse && landuse_class(&way.tags).is_some());
        if !interesting {
            continue;
        }
        if let Some(bbox) = &keep_box {
            if !way_in_bbox(way, file, bbox) {
                continue;
            }
        }
        consider(way, &mut min_lat, &mut min_lon, &mut max_lat, &mut max_lon);
    }
    if !min_lat.is_finite() || !min_lon.is_finite() {
        // Nothing to import: anchor on the frame, or on the file's own bounds.
        let bounds = frame_box.unwrap_or(GeoBbox::new(0.0, 0.0, 0.0, 0.0));
        min_lat = bounds.min_lat_deg;
        min_lon = bounds.min_lon_deg;
        max_lat = bounds.max_lat_deg;
        max_lon = bounds.max_lon_deg;
    }
    let origin =
        frame_origin.unwrap_or_else(|| origin_of(GeoBbox::new(min_lat, min_lon, max_lat, max_lon)));
    let projection = Projection::new(origin);
    let points: Vec<Vec3> = file
        .nodes
        .iter()
        .map(|n| projection.to_enu_vec3(n.lat, n.lon, 0.0))
        .collect();
    let geodetic_extent = GeoBbox::new(min_lat, min_lon, max_lat, max_lon);

    // What the box does to a building or land-use ring, in world metres: the projection is
    // monotone, so the geodetic box maps to an axis-aligned rectangle. Under
    // `BboxClip::Clip` the ring is cut to the box, so the world's extent really is the
    // request plus the margin; under `KeepWhole` a ring with any vertex inside is kept
    // entire, as it always was.
    let area_box = keep_box.map(|b| {
        let (x0, y0) = projection.to_enu(b.min_lat_deg, b.min_lon_deg);
        let (x1, y1) = projection.to_enu(b.max_lat_deg, b.max_lon_deg);
        ClipBox {
            min_x: x0,
            min_y: y0,
            max_x: x1,
            max_y: y1,
        }
    });
    let cut_polygons = matches!(options.bbox_clip, BboxClip::Clip { .. });
    // Returns the ring to import and whether the box actually cut it — which is "some
    // vertex was outside", not "the vertex count changed": clipping a closed ring drops
    // its repeated last point whether or not anything was outside.
    let keep = move |ring: &[Vec3]| -> Option<(Vec<Vec3>, bool)> {
        match area_box {
            None => Some((ring.to_vec(), false)),
            Some(box_) if cut_polygons => {
                let cut = ring.iter().any(|p| !box_.contains(*p));
                clip_ring_to_box(ring, &box_).map(|kept| (kept, cut))
            }
            Some(box_) => ring
                .iter()
                .any(|p| box_.contains(*p))
                .then(|| (ring.to_vec(), false)),
        }
    };
    // The geometric clip (V5), in world metres: the requested box grown by the margin.
    let clip_box = match (options.bbox, options.bbox_clip) {
        (Some(b), BboxClip::Clip { .. }) => {
            let (x0, y0) = projection.to_enu(b.min_lat_deg, b.min_lon_deg);
            let (x1, y1) = projection.to_enu(b.max_lat_deg, b.max_lon_deg);
            Some(ClipBox {
                min_x: x0 - margin_m,
                min_y: y0 - margin_m,
                max_x: x1 + margin_m,
                max_y: y1 + margin_m,
            })
        }
        _ => None,
    };
    // Synthetic ids for the boundary junctions the clip creates, counting down from below
    // every id the extract carries. `file.nodes` is sorted, so the first is the smallest.
    let mut next_synthetic = file.nodes.first().map_or(0, |n| n.id).min(0);

    // --- stages 4 and 5: split and collapse --------------------------------------
    let segments = split_ways(
        &plans,
        file,
        &points,
        clip_box.as_ref(),
        &mut next_synthetic,
        report,
    );
    report.counts.segments_before_collapse = segments.len() as u64;
    let protected = restriction_via_nodes(file);
    let (segments, collapsed) = if options.simplify.collapse_trivial_junctions {
        collapse_trivial_junctions(segments, &plans, &protected, report)
    } else {
        let mut kept = segments;
        kept.sort_by_key(|s| s.key);
        (kept, 0)
    };
    report.counts.segments_after_collapse = segments.len() as u64;
    report.counts.junctions_collapsed = collapsed;

    // --- stages 6 and 7: lanes, junctions, movements ------------------------------
    let mut net = build_network(&segments, &plans, file, &points, options, report)?;
    let mut direct = build_movements(&mut net, &plans, report);
    let (restrictions, _banned) = apply_restrictions(&mut net, &mut direct, file, report);
    report.counts.restrictions_applied = restrictions;

    let mut connections: Vec<Connection> = direct
        .iter()
        .map(|m| Connection {
            from_lane: m.from_lane,
            to_lane: m.to_lane,
            via: None,
            direction: m.turn,
            permitted: m.permitted,
        })
        .collect();
    for j in 0..net.junctions.len() {
        for movement in &net.movements[j] {
            connections.push(Connection {
                from_lane: movement.from_lane,
                to_lane: movement.to_lane,
                via: Some(movement.internal),
                direction: movement.turn,
                permitted: movement.permitted,
            });
            connections.push(Connection {
                from_lane: movement.internal,
                to_lane: movement.to_lane,
                via: None,
                direction: movement.turn,
                permitted: movement.permitted,
            });
        }
        net.junctions[j].conflicts = conflict_matrix(&net.movements[j], &net.lanes);
    }
    connections.extend(connect_soft_lanes(&net));
    assign_control(&mut net, &plans, file);

    // --- stage 8: signals ----------------------------------------------------------
    let signals = synthesise_signals(&mut net, file, &points, options, report);
    report.counts.signalised_junctions = signals.len() as u64;

    // V9: 77.6 % of the Phase 1 world's junctions were footway intersections in the
    // sidewalk mesh, so both the junction count and the signalisation share meant
    // something other than what a reader would assume. An arm is a distinct *segment*
    // rather than a distinct edge, so a two-way street counts once.
    for j in 0..net.junctions.len() {
        let arms: BTreeSet<usize> = net.incoming[j]
            .iter()
            .chain(&net.outgoing[j])
            .filter(|e| net.edge_info[**e].family == WayFamily::Motor)
            .map(|e| net.edge_info[*e].segment)
            .collect();
        if arms.is_empty() {
            continue;
        }
        report.counts.road_junctions += 1;
        if arms.len() >= 3 {
            report.counts.major_road_junctions += 1;
            if matches!(net.junctions[j].control, JunctionControl::Signalised { .. }) {
                report.counts.signalised_major_junctions += 1;
            }
        }
    }

    // --- stage 9: crossings, buildings, land use ------------------------------------
    let crossings = if options.layers.crossings {
        build_crossings(file, &points, &net, options, report)
    } else {
        Vec::new()
    };
    let buildings = if options.layers.buildings {
        build_buildings(file, &points, &keep, options, &mut symbols, report)
    } else {
        Vec::new()
    };
    let landuse = if options.layers.landuse {
        build_landuse(file, &points, &keep, &mut symbols, report)
    } else {
        Vec::new()
    };

    // Junction lane lists are documented as being in id order.
    for junction in &mut net.junctions {
        junction.incoming.sort_unstable();
        junction.incoming.dedup();
        junction.outgoing.sort_unstable();
        junction.outgoing.dedup();
    }
    build_junction_shapes(&mut net);

    // --- counts ---------------------------------------------------------------------
    report.counts.junctions = net.junctions.len() as u64;
    report.counts.edges = net.edges.len() as u64;
    report.counts.lanes = net.lanes.len() as u64;
    report.counts.connections = connections.len() as u64;
    report.counts.banned_connections = connections.iter().filter(|c| !c.permitted).count() as u64;
    report.counts.crossings = crossings.len() as u64;
    report.counts.buildings = buildings.len() as u64;
    report.counts.landuse_zones = landuse.len() as u64;
    for lane in &net.lanes {
        if lane.admits(ClassMask::MOTOR_TRAFFIC) {
            report.counts.drivable_lanes += 1;
        }
        match lane.kind {
            LaneKind::Internal => report.counts.internal_lanes += 1,
            LaneKind::Sidewalk => report.counts.sidewalk_lanes += 1,
            LaneKind::Cycle => report.counts.cycle_lanes += 1,
            _ => {}
        }
    }
    // --- stage 10: provenance ---------------------------------------------------------
    let provenance = build_provenance(
        options,
        origin,
        frame,
        geodetic_extent,
        report,
        collapsed,
        net.junctions.len() as u64,
    );

    let roads = RoadNetwork::new(net.lanes, net.edges, net.junctions, connections, crossings)?;
    let mut builder = World::builder(origin)
        .roads(roads)
        .buildings(buildings)
        .landuse(landuse)
        .signals(signals)
        .default_env(options.import.default_env)
        .symbols(symbols)
        .provenance(provenance)
        .index_options(options.import.index_options);
    if let Some(terrain) = options.terrain.clone() {
        builder = builder.terrain(terrain);
    }
    let world = builder.build()?;

    // The extent is measured from the world that came out, not from the source scan, so
    // that "extent against the requested box" is a fact rather than an estimate (V3/V5).
    if !world.bbox.is_empty() {
        report.extent_m = Some((
            quantise(world.bbox.max.x - world.bbox.min.x, Q_POSITION_M),
            quantise(world.bbox.max.y - world.bbox.min.y, Q_POSITION_M),
        ));
        let (low_lat, low_lon) = projection.to_geodetic(world.bbox.min.x, world.bbox.min.y);
        let (high_lat, high_lon) = projection.to_geodetic(world.bbox.max.x, world.bbox.max.y);
        report.bbox = Some(GeoBbox::new(
            quantise(low_lat, Q_DEGREES),
            quantise(low_lon, Q_DEGREES),
            quantise(high_lat, Q_DEGREES),
            quantise(high_lon, Q_DEGREES),
        ));
    } else {
        report.bbox = Some(geodetic_extent);
    }
    Ok(world)
}

/// Gives every junction its area: the convex hull of the lane ends that meet there.
///
/// 04-models.md §1.2 asks for "a polygon area from the incoming lane ends", which is what
/// this is: every approach lane's last point, every departure lane's first point and the
/// junction's own node, hulled. A junction with fewer than three distinct such points —
/// the end of a cul-de-sac — keeps an empty shape, which the model and the wire payload
/// both allow.
fn build_junction_shapes(net: &mut Net) {
    for j in 0..net.junctions.len() {
        let mut points: Vec<Vec3> = vec![net.junctions[j].position];
        for lane in &net.junctions[j].incoming {
            points.push(net.lanes[lane.as_usize()].end());
        }
        for lane in &net.junctions[j].outgoing {
            points.push(net.lanes[lane.as_usize()].start());
        }
        net.junctions[j].shape = convex_hull(&points);
    }
}

/// The osm2streets simplifications that were asked for and are not implemented.
fn skipped_simplifications(simplify: &OsmSimplifications) -> Vec<String> {
    let mut out = Vec::new();
    if simplify.merge_dual_carriageways {
        out.push("merge-dual-carriageways".to_string());
    }
    if simplify.snap_parallel_footways {
        out.push("snap-parallel-footways".to_string());
    }
    // Dog-leg merging is on the osm2streets list and is not offered at all, so it is
    // always reported: a reader of the provenance should see the whole list.
    out.push("merge-dog-leg-junctions".to_string());
    out
}

/// Builds the provenance record: every transformation with its parameters (invariant
/// I-W3), the ODbL licence and its attribution, and what the import dropped.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn build_provenance(
    options: &OsmOptions,
    origin: GeoOrigin,
    frame: FrameRule,
    extent: GeoBbox,
    report: &ImportReport,
    collapsed: u64,
    junctions: u64,
) -> WorldProvenance {
    let mut provenance = WorldProvenance::new(
        WorldSourceKind::Osm,
        format!("sha256:{}", report.source_sha256),
        options.import.imported_at.clone(),
        origin,
    );
    provenance.source_bbox = Some(options.bbox.unwrap_or(extent));
    provenance
        .tool_versions
        .insert(MODEL_ID.to_string(), MODEL_VERSION.to_string());
    provenance.record(
        Transformation::new("osm-import")
            .with("source", report.source_id.clone())
            .with("source_bytes", report.source_bytes)
            .with("drivable", options.layers.drivable)
            .with("pedestrian", options.layers.pedestrian)
            .with("cycle", options.layers.cycle)
            .with("buildings", options.layers.buildings)
            .with("landuse", options.layers.landuse)
            .with("crossings", options.layers.crossings),
    );
    provenance.record(
        Transformation::new("local-tangent-plane")
            .with("projection", Projection::NAME)
            .with("origin_lat_deg", origin.lat_deg)
            .with("origin_lon_deg", origin.lon_deg)
            .with("frame", frame.label())
            .with("rule", frame.rule()),
    );
    if let Some(bbox) = options.bbox {
        provenance.record(
            Transformation::new("bbox-clip")
                .with("min_lon", bbox.min_lon_deg)
                .with("min_lat", bbox.min_lat_deg)
                .with("max_lon", bbox.max_lon_deg)
                .with("max_lat", bbox.max_lat_deg)
                .with("mode", options.bbox_clip.label())
                .with(
                    "margin_m",
                    quantise(options.bbox_clip.margin_m(), Q_POSITION_M),
                )
                .with("ways_cut", report.counts.ways_clipped)
                .with("runs_kept", report.counts.clipped_runs)
                .with("polygons_cut", report.counts.polygons_clipped)
                .with(
                    "rule",
                    match options.bbox_clip {
                        BboxClip::KeepWhole => {
                            "a way is kept whole when any of its nodes is inside"
                        }
                        BboxClip::Clip { .. } => {
                            "way geometry is cut at the box grown by margin_m, the \
                             crossing point interpolated, and each run inside split at \
                             its own junction nodes; building and land-use rings are cut \
                             to the same box"
                        }
                    },
                ),
        );
    }
    provenance.record(
        Transformation::new("split-ways")
            .with(
                "rule",
                "cut at nodes shared by two or more ways of the same family; interior \
                 nodes stay as shape",
            )
            .with("segments", report.counts.segments_before_collapse),
    );
    provenance.record(
        Transformation::new("collapse-trivial-junctions")
            .with("enabled", options.simplify.collapse_trivial_junctions)
            .with("junctions_removed", collapsed)
            .with("junctions_kept", junctions),
    );
    for name in &report.skipped_simplifications {
        provenance.record(
            Transformation::new("not-applied")
                .with("simplification", name.clone())
                .with("reason", "not implemented (04-models.md §1.2)"),
        );
    }
    if let Some(tolerance) = options.import.simplify_tolerance_m {
        provenance.record(
            Transformation::new("simplify")
                .with("algorithm", "ramer-douglas-peucker")
                .with("tolerance_m", tolerance),
        );
    }
    provenance.record(
        Transformation::new("lane-layout")
            .with(
                "lane_width_m",
                options.lane_width_m.map_or(serde_json::Value::Null, |w| {
                    serde_json::Value::from(quantise(w, Q_POSITION_M))
                }),
            )
            .with("sidewalk_width_m", options.sidewalk_width_m)
            .with("cycleway_width_m", options.cycleway_width_m)
            .with("rule", "lanes offset left to right from the way centreline")
            .with(
                "width_rule",
                "width:lanes, else width / lanes, else the lane_width_m option, else the \
                 class default",
            )
            .with("width_source", LANE_WIDTH_SOURCE)
            .with("widths_from_tag", report.counts.widths_tagged)
            .with("widths_defaulted", report.counts.widths_defaulted),
    );
    // The preset is `Some` here: `OsmOptions::validate` refused the import otherwise.
    // `map_or` rather than `unwrap`, because a panic is never the way to say so.
    let audit = &report.speed_audit;
    provenance.record(
        Transformation::new("class-defaults")
            .with(
                "preset",
                options
                    .highway_preset
                    .map_or("(none selected)", HighwayPreset::label),
            )
            .with(
                "source",
                options
                    .highway_preset
                    .map_or("(none selected)", HighwayPreset::source),
            )
            .with(
                "row_sources",
                options
                    .highway_preset
                    .map(|p| p.row_sources().join(" | "))
                    .unwrap_or_default(),
            )
            .with("speeds_from_tag", report.counts.speeds_tagged)
            .with("speeds_defaulted", report.counts.speeds_defaulted)
            .with("audit_tagged_p95_mps", audit.tagged_p95_mps)
            .with("audit_threshold_mps", audit.threshold_mps)
            .with("audit_lanes_far_above", audit.far_above_lanes)
            .with("audit_share_far_above", audit.far_above_share)
            .with("audit_jurisdiction_mismatch", audit.fired)
            .with(
                "rule",
                "maxspeed where the way states one and the family is motor traffic, else \
                 the preset's class default; a maxspeed on a footway, path or cycleway is \
                 ignored and counted",
            ),
    );
    provenance.record(
        Transformation::new("lane-offset-repair")
            .with(
                "rule",
                "inverted mitre vertices pruned and self-crossings cut out, so a lane \
                 centreline never crosses itself",
            )
            .with("lanes_repaired", report.counts.lanes_repaired)
            .with(
                "short_lane_threshold_m",
                quantise(options.min_useful_lane_m, Q_POSITION_M),
            )
            .with("short_driving_lanes", report.counts.short_driving_lanes)
            .with(
                "short_lane_policy",
                "kept and counted as short-driving-lane: dropping one disconnects \
                 whatever is on the other side",
            ),
    );
    provenance.record(
        Transformation::new("junction-trim")
            .with(
                "rule",
                "each approach trimmed by the widest incident half-carriageway",
            )
            .with("min_radius_m", MIN_JUNCTION_RADIUS_M)
            .with("max_radius_m", MAX_JUNCTION_RADIUS_M),
    );
    provenance.record(
        Transformation::new("turn-inference")
            .with(
                "rule",
                "turn:lanes where usable, else right from the rightmost lane, \
                          left and u-turn from the leftmost, straight from any",
            )
            .with("restrictions_applied", report.counts.restrictions_applied),
    );
    provenance.record(
        Transformation::new("signal-guess")
            .with("guess_signals_m", options.import.guess_signals_m)
            .with("cycle_s", options.signals.cycle_s)
            .with("green_s", options.signals.green_s)
            .with("all_red_s", options.signals.all_red_s)
            .with("yellow_rule", "ITE y = t + v / (2a)")
            .with("yellow_reaction_s", options.signals.yellow_reaction_s)
            .with(
                "yellow_min_decel_mps2",
                options.signals.yellow_min_decel_mps2,
            )
            .with("source", "04-models.md §2.3, netconvert --tls.* defaults"),
    );
    provenance.record(
        Transformation::new("height-default")
            .with(
                "rule",
                "height tag, else building:levels x metres_per_level, else default",
            )
            .with("metres_per_level", options.import.metres_per_level)
            .with("default_height_m", options.import.default_height_m)
            .with("tagged", report.counts.heights_tagged)
            .with("from_levels", report.counts.heights_from_levels)
            .with("defaulted", report.counts.heights_defaulted)
            .with("calibration", "TODO: calibrate (04-models.md §1.3)"),
    );
    provenance.record(
        Transformation::new("building-parts")
            .with(
                "rule",
                "OSM Simple 3D Buildings: a building=* outline is one structure and one \
                 obstacle; a building:part=* inside an outline is folded into it; a part \
                 with no outline over it is kept as a building and counted",
            )
            .with("source", "OSM Simple 3D Buildings, via 04-models.md §1.3")
            .with("parts_merged", report.counts.building_parts_merged)
            .with("parts_orphan", report.counts.building_parts_orphan)
            .with(
                "parts_geometry",
                "dropped: the world model carries no per-part volume",
            ),
    );
    provenance.record(
        Transformation::new("subsurface-exclusion")
            .with(
                "rule",
                "a polygon with location=underground, underground=yes, a negative layer, \
                 a negative building:min_level and no height, or a station or \
                 public-transport area with no building tag, is not an above-ground \
                 obstacle and is excluded",
            )
            .with("excluded", report.counts.buildings_subsurface),
    );
    provenance.record(
        Transformation::new("spire-cap")
            .with(
                "rule",
                "where roof:shape tapers to a point (spire, cone, pyramidal, onion) or \
                 roof:height exceeds roof_spire_fraction x height, the obstacle height is \
                 cut back to height - roof:height, the structural top; the tip is not kept",
            )
            .with("source", "OSM Simple 3D Buildings roof:shape vocabulary")
            .with(
                "roof_spire_fraction",
                quantise(options.roof_spire_fraction, 1e-4),
            )
            .with("capped", report.counts.buildings_spire_capped)
            .with(
                "calibration",
                "TODO: calibrate (no source states a threshold)",
            ),
    );
    provenance.record(
        Transformation::new("terrain")
            .with(
                "rule",
                if options.terrain.is_some() {
                    "caller-supplied grid"
                } else {
                    "none: flat at z = 0 (Phase 1)"
                },
            )
            .with("source", "04-models.md §1.4"),
    );
    provenance.record(
        Transformation::new("quantise")
            .with("position_m", Q_POSITION_M)
            .with("height_m", crate::quant::Q_HEIGHT_M)
            .with("speed_mps", crate::quant::Q_SPEED_MPS)
            .with("time_s", Q_TIME_S)
            .with("degrees", Q_DEGREES),
    );

    for layer in ["roads", "buildings", "landuse", "crossings"] {
        provenance.layers.push(LayerLicence::with_attribution(
            layer,
            OSM_LICENCE,
            OSM_ATTRIBUTION,
        ));
    }
    provenance.notes.push(
        "An OSM-derived world is very likely a Derivative Database under the ODbL, not a \
         Produced Work; export obligations are in 08-measurement-and-data.md §9."
            .to_string(),
    );
    provenance.notes.push(
        "Signal plans are synthesised: an OSM extract carries signal presence only \
         (04-models.md §1.2)."
            .to_string(),
    );
    if !report.skipped_simplifications.is_empty() {
        provenance.notes.push(format!(
            "Simplifications not implemented: {}.",
            report.skipped_simplifications.join(", ")
        ));
    }
    for (kind, count) in &report.anomalies {
        provenance.record_dropped(kind.label(), *count);
    }
    if !options.import.keep_building_holes {
        provenance.record_dropped("building_holes", report.counts.building_holes);
    }
    provenance
}

/// The junctions of `world` that have at least one drivable arm — the ones a vehicular
/// model means by "junction" (V9).
///
/// 77.6 % of the Phase 1 world's 3 421 junctions have **no** drivable arm at all: they are
/// the intersections of the sidewalk mesh, carried in the same collection because a
/// pedestrian graph is part of the same world. Every consumer that walked
/// [`crate::model::RoadNetwork::junctions`] therefore paid 4.5× for what it wanted, and
/// any share quoted against that denominator — the import report's own signalisation
/// figure included — understated itself by the same factor.
///
/// This is a linear scan over the edges, in id order, and the result is in id order, so
/// it is cheap and deterministic. It lives in this module because the fix is the
/// importer's, not the model's: the model is right to keep one junction collection.
///
/// ```no_run
/// use v2xw_world::osm::{HighwayPreset, OsmOptions, import_osm, road_junction_ids};
/// let options = OsmOptions::default().highway_preset(HighwayPreset::UrbanUsNyc);
/// let (world, report) = import_osm("worlds/cache/city.osm.xml", &options)?;
/// let roads = road_junction_ids(&world);
/// assert_eq!(roads.len() as u64, report.counts.road_junctions);
/// # Ok::<(), v2xw_world::WorldError>(())
/// ```
pub fn road_junction_ids(world: &World) -> Vec<JunctionId> {
    let mut has_arm = vec![false; world.roads.junctions().len()];
    for edge in world.roads.edges() {
        if edge.road_class == RoadClass::Internal {
            continue;
        }
        let drivable = edge
            .lanes
            .iter()
            .any(|l| world.roads.lane(*l).admits(ClassMask::MOTOR_TRAFFIC));
        if !drivable {
            continue;
        }
        for j in [edge.from, edge.to] {
            if let Some(slot) = has_arm.get_mut(j.as_usize()) {
                *slot = true;
            }
        }
    }
    world
        .roads
        .junctions()
        .iter()
        .filter(|j| has_arm.get(j.id.as_usize()).copied().unwrap_or(false))
        .map(|j| j.id)
        .collect()
}

// ---------------------------------------------------------------------------
// The plug-in seam and the model card
// ---------------------------------------------------------------------------

/// The [`WorldSource`] plug-in wrapper around [`import_osm`].
///
/// It holds the OSM-specific options, because [`WorldSource::build`] is handed only the
/// common [`ImportOptions`]; a scenario that wants a different lane width or a different
/// signal cycle constructs the source with them.
#[derive(Debug, Clone, Default)]
pub struct OsmSource {
    options: OsmOptions,
}

impl OsmSource {
    /// A source with the default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// A source with the given OSM options. [`WorldSource::build`] overrides the common
    /// [`OsmOptions::import`] block with the options it is handed, and the bounding box
    /// with the one in the specification, so that a scenario file stays authoritative.
    pub fn with_options(options: OsmOptions) -> Self {
        Self { options }
    }

    /// The options this source will use.
    pub fn options(&self) -> &OsmOptions {
        &self.options
    }
}

impl WorldSource for OsmSource {
    fn card(&self) -> ModelCard {
        card()
    }

    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World> {
        match src {
            WorldSourceSpec::OsmXml { path, bbox } => {
                let mut options = self.options.clone();
                options.import = opts.clone();
                if bbox.is_some() {
                    options.bbox = *bbox;
                }
                let (world, _report) = import_osm(path, &options)?;
                Ok(world)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

/// The model card of `world/source/osm` (03-interfaces.md §12).
///
/// Every parameter the importer reads appears here with its unit, its default and where
/// that default comes from. The two height parameters and the class default table are
/// marked `todo-calibrate` with a plan, because no source consulted states them as
/// measurements: that is what registry rule R1 requires, and what puts them on the
/// generated "todo-calibrate" page rather than letting them pass as facts.
pub fn card() -> ModelCard {
    let osm2streets = || {
        Source::new(
            SourceKind::Code,
            "osm2streets README (lane inference and the simplification list), via \
             04-models.md §1.2",
        )
    };
    let netconvert = || {
        Source::new(
            SourceKind::Code,
            "SUMO netconvert option defaults (--junctions.join 10 m, --tls.guess-signals \
             25 m, --tls.cycle.time 90 s, --tls.green.time 31 s, --tls.allred.time 0 s, \
             --tls.yellow.min-decel 3 m/s^2), via 04-models.md §1.2 and §2.3",
        )
    };
    let osm_wiki = || {
        Source::new(
            SourceKind::Dataset,
            "OSM Simple 3D Buildings (height, building:levels, min_height, \
             building:min_level), via 04-models.md §1.3",
        )
    };
    let fhwa = || {
        Source::new(
            SourceKind::Standard,
            "FHWA Signal Timing Manual 2008 Ch. 5 (amber 3-6 s, all-red guidance), via \
             04-models.md §2.3",
        )
    };
    let lane_geometry = || Source::new(SourceKind::Standard, LANE_WIDTH_SOURCE);
    let no_default_preset = || {
        Source::new(
            SourceKind::Code,
            "no default: a class default speed is a statement about a jurisdiction, so \
             the scenario names a preset and OsmOptions::validate refuses the import \
             until it does (V4/W1)",
        )
    };
    let speed_audit_thresholds = || {
        Source::new(
            SourceKind::Code,
            "this importer's own thresholds for calling a preset out of jurisdiction: a \
             class default above 1.25x the source's tagged 95th percentile, on at least \
             5 % of the drivable lanes. They set when a warning is printed and change \
             nothing about the world",
        )
    };
    let nyc_limits = || Source::new(SourceKind::Standard, HighwayPreset::UrbanUsNyc.source());
    let vehicle_length = || {
        Source::new(
            SourceKind::Code,
            "SUMO default vType `passenger` length 5.0 m: a lane shorter than one vehicle \
             cannot hold one, which is the threshold short-driving-lane is counted against",
        )
    };
    let todo = |name: &str, unit: &str, default: serde_json::Value, plan: &str| {
        let mut p = Parameter::new(
            name.to_string(),
            unit.to_string(),
            default,
            Source::todo_calibrate(format!("world/source/osm {name}")),
        );
        p.calibration = Some(plan.to_string());
        p
    };
    let width_table: serde_json::Value = serde_json::Value::Object(
        CLASS_LANE_WIDTH_M
            .iter()
            .map(|(class, w)| (class.label().to_string(), serde_json::Value::from(*w)))
            .collect(),
    );
    let speed_table: serde_json::Value = serde_json::Value::Object(
        HighwayPreset::ALL
            .into_iter()
            .map(|preset| {
                let rows: serde_json::Value = serde_json::Value::Object(
                    preset
                        .table()
                        .iter()
                        .map(|row| {
                            (
                                row.key.to_string(),
                                serde_json::json!({
                                    "speed_mps": row.speed_mps,
                                    "lanes": row.lanes,
                                    "source": row.speed_source,
                                }),
                            )
                        })
                        .collect(),
                );
                (preset.label().to_string(), rows)
            })
            .collect(),
    );
    let parameters = vec![
        // V6/W2: the global width is now the last resort, not the only rule, so its
        // default is "unset" and the table below is what an untagged way gets.
        Parameter::new(
            "lane_width_m",
            "m",
            serde_json::Value::Null,
            lane_geometry(),
        ),
        Parameter::new("lane_width_class_m", "m", width_table, lane_geometry()),
        // V4/W1: the class default table is a named preset, and every row cites its own
        // speed. The choice of preset is the scenario's, and it is recorded in the
        // report and the provenance. Its default is `null` because it HAS no default:
        // the import fails until a jurisdiction is named.
        Parameter::new(
            "highway_preset",
            "-",
            serde_json::Value::Null,
            no_default_preset(),
        ),
        // The audit that catches a preset from the wrong jurisdiction.
        Parameter::new(
            "speed_audit_factor",
            "-",
            SPEED_AUDIT_FACTOR.into(),
            speed_audit_thresholds(),
        ),
        Parameter::new(
            "speed_audit_share",
            "-",
            SPEED_AUDIT_SHARE.into(),
            speed_audit_thresholds(),
        ),
        Parameter::new("highway_preset_table", "-", speed_table, nyc_limits()),
        Parameter::new("min_useful_lane_m", "m", 5.0.into(), vehicle_length()),
        Parameter::new("sidewalk_width_m", "m", 2.0.into(), osm2streets()),
        Parameter::new("cycleway_width_m", "m", 1.5.into(), osm2streets()),
        Parameter::new("junction_join_m", "m", 10.0.into(), netconvert()),
        Parameter::new("guess_signals_m", "m", 25.0.into(), netconvert()),
        Parameter::new("cycle_s", "s", 90.0.into(), netconvert()),
        Parameter::new("green_s", "s", 31.0.into(), netconvert()),
        Parameter::new("all_red_s", "s", 0.0.into(), netconvert()),
        Parameter::new("yellow_min_decel_mps2", "m/s^2", 3.0.into(), netconvert()),
        Parameter::new("yellow_min_s", "s", 3.0.into(), fhwa()),
        Parameter::new("yellow_max_s", "s", 6.0.into(), fhwa()),
        Parameter::new("height_tag_rule", "-", "height".into(), osm_wiki()),
        // V1: which outline a part is folded into, and on what evidence.
        Parameter::new(
            "building_part_parent_rule",
            "-",
            "greatest shared footprint area, at least 0.50 of the part; among outlines \
             covering 0.99 or more of it, the one nested in the others, then the one \
             already subdivided into parts, then the lower OSM id"
                .into(),
            Source::new(
                SourceKind::Code,
                "OSM Simple 3D Buildings says a building:part subdivides the building=* \
                 outline it lies in, which is a statement about two polygons, so the \
                 parent is chosen by clipped overlap area rather than by testing one \
                 point of the part. The thresholds are read off the Phase 1 Manhattan \
                 extract, where they have nothing near them: of 4 373 parts, every one \
                 that has a parent is 0.934 or more covered by it and the runner-up is \
                 0.32 or less, and of the nine parts that more than one outline claims \
                 the covering fraction is either 1.0000 to four decimals or 0.9553 or \
                 less. The tie-breaks are each forced by a real case: the United Nations \
                 Secretariat inside the United Nations Headquarters campus for nesting, \
                 Rose Hill against the 156 m² building beside it for subdivision",
            ),
        ),
        // V1: what "folded into its outline" does to the height, and what it costs.
        Parameter::new(
            "building_parts_as_max_height",
            "-",
            "outline height = max(own height, tallest building:part folded into it)".into(),
            Source::new(
                SourceKind::Code,
                "OSM Simple 3D Buildings says a building:part subdivides its outline, so \
                 the structure's top is its tallest part's top. Collapsing the parts to \
                 that one number is this importer's simplification: Building has no \
                 sub-volume field and the vwp-v1 building record has no min_height \
                 column, so a stepped tower is one prism and the setback above a podium \
                 is filled in",
            ),
        ),
        todo(
            "metres_per_level",
            "m",
            3.0.into(),
            "the OSM wiki states no standard level-to-metre factor; fit it by regressing \
             Microsoft-estimated heights on OSM building:levels over the Phase 2 city \
             boxes and record the fit and its residual (04-models.md §1.3)",
        ),
        todo(
            "default_height_m",
            "m",
            10.0.into(),
            "same study as metres_per_level, per land-use class (04-models.md §1.3)",
        ),
        todo(
            "highway_class_defaults",
            "-",
            "SUMO osmNetconvert.typ.xml".into(),
            "the lane counts and speeds are reproduced from SUMO's OSM type map and were \
             not re-verified during this build; replace the speeds with the measured \
             maxspeed distribution of the Phase 2 city boxes (04-models.md §1.2)",
        ),
        todo(
            "bbox_clip_margin_m",
            "m",
            (2.0 * MAX_JUNCTION_RADIUS_M).into(),
            "one junction diameter, taken as twice the 25 m maximum junction trim radius, \
             so that a junction sitting on the requested boundary is still trimmed with \
             its true radius; measure how far a boundary junction's arms actually reach \
             over the Phase 2 city boxes and set it to the 99th percentile",
        ),
        todo(
            "roof_spire_fraction",
            "-",
            0.5.into(),
            "the roof:shape vocabulary names the shapes that taper to a point and needs no \
             threshold; this is the second trigger, for a roof that states no shape, and \
             no source states the fraction at which roof:height stops being building mass. \
             Fit it from the roof:height / height distribution of the Phase 2 city boxes, \
             split by roof:shape, and record the knee",
        ),
        todo(
            "crossing_width_m",
            "m",
            4.0.into(),
            "measure painted crossing widths from the Phase 2 city extracts, where OSM \
             crossing ways carry a width tag",
        ),
        todo(
            "signal_head_height_m",
            "m",
            5.0.into(),
            "measure mast-arm mounting heights from three street-level imagery samples \
             per Phase 2 city and record the median",
        ),
    ];

    ModelCard {
        tier: vec![Tier::Abstract, Tier::Medium, Tier::High],
        equations: vec![
            Equation::new(
                "lane offset",
                "d_k = -(W/2 - (k + 0.5) w) from the way centreline, positive to the left \
                 of travel, W the carriageway width and k the lane index from the right",
            ),
            Equation::new(
                "junction trim",
                "each approach is cut back by r_j = clamp(max incident W/2, 1 m, 25 m)",
            ),
            Equation::new(
                "connector",
                "quadratic Bezier through the meeting point of the two tangent lines, \
                 sampled at 7 points",
            ),
            Equation::new("amber", "y = t + v / (2 a), clamped to [3, 6] s"),
            Equation::new(
                "building height",
                "height tag; else building:levels x metres_per_level; else default_height_m; \
                 then, where roof:height > roof_spire_fraction x height, cut back to \
                 height - roof:height",
            ),
            Equation::new(
                "lane width",
                "mean of width:lanes; else width / total lanes; else the lane_width_m \
                 option; else the class default",
            ),
        ],
        parameters,
        assumptions: vec![
            "Right-hand traffic: lane 0 is the rightmost in the direction of travel, and \
             a left turn is the movement that crosses opposing traffic."
                .to_string(),
            "A way's centreline is the centre of its carriageway, so lanes are laid out \
             symmetrically about it."
                .to_string(),
            "A node shared by two or more ways of the same family is a junction; every \
             other node is shape."
                .to_string(),
            "Signal plans are invented: the source carries presence only. Every plan is \
             two-phase, pre-timed, uncoordinated (offset 0) and permissive for turns that \
             cross opposing traffic."
                .to_string(),
            "The ground is flat at z = 0. `layer`, `bridge` and `tunnel` are read and keep \
             two ways from being merged, but do not separate geometry vertically: ways on \
             different levels connect only where they share a node, which they do not."
                .to_string(),
        ],
        limitations: vec![
            "Dual carriageways are imported as two one-way streets, because the \
             sausage-link merge is not implemented; a divided boulevard therefore has two \
             centrelines and a median that is not modelled."
                .to_string(),
            "A class default speed is jurisdictional, so there is NO default preset: an \
             import is refused until the scenario names one. `sumo-german` is SUMO's \
             free-flow design speeds and keeps a netconvert comparison reproducible, and \
             is not an urban legal limit; `urban-us-nyc` is the NYC statutory default. \
             The report and the provenance name the preset, count the lanes that took a \
             default, and warn when those defaults sit far above the limits the source \
             itself states."
                .to_string(),
            "`building:part` geometry is not kept, only its HEIGHT. A part inside a \
             `building=*` outline is folded into the outline so that one structure is one \
             obstacle, and the outline's height becomes the taller of its own and the \
             tallest part inside it (`HeightSource::FromParts`). The per-part volume is \
             dropped, because the world model has no field for a sub-volume and the \
             vwp-v1 building record has no `min_height` column to carry one: a stepped \
             tower is therefore ONE prism as tall as its tallest part, so an obstacle \
             model sees the setback above a podium filled in and over-estimates mass at \
             high altitude. A part with no outline over it is kept as a building and \
             counted."
                .to_string(),
            "A polygon the tags place below ground, or that is a station or \
             public-transport area with no building tag, is not imported at all — not as a \
             zero-height obstacle and not as land use."
                .to_string(),
            "A driving lane shorter than `min_useful_lane_m` is kept and counted rather \
             than merged into its neighbour: merging changes the topology a turn \
             restriction and a conflict matrix were built against, and dropping it \
             disconnects whatever is on the other side."
                .to_string(),
            "A movement whose two lane ends are within a millimetre has no connector lane \
             and therefore no conflict-matrix row, because the matrix is indexed by \
             connector. It is still subject to turn restrictions, and it is counted as \
             `zero-length-connector`."
                .to_string(),
            "Footways parallel to a road are imported where they are mapped, and are not \
             snapped to the road; a city that maps sidewalks both ways will have both."
                .to_string(),
            "Dog-leg junctions are imported as two junctions a few metres apart.".to_string(),
            "A `via`-way turn restriction is counted and not applied: it spans more than \
             one junction and has no single movement to ban."
                .to_string(),
            "Two opposing left turns are recorded as conflicting with no precedence, \
             because neither the turn ranking nor priority-to-the-right settles them."
                .to_string(),
        ],
        ignores: vec![
            "Terrain: no DEM is read, and every z is 0 (04-models.md §1.4).".to_string(),
            "Traffic signal timing, phase counts and coordination, which the source does \
             not carry."
                .to_string(),
            "Bus lanes, parking lanes, turn pockets and lane widths per lane: every lane \
             of a way gets the same width and access."
                .to_string(),
            "Public transport routes, barriers, kerbs and traffic calming.".to_string(),
        ],
        sources: vec![
            osm2streets(),
            netconvert(),
            osm_wiki(),
            fhwa(),
            lane_geometry(),
            nyc_limits(),
            vehicle_length(),
        ],
        validation: Validation::new(ValidationStatus::Unvalidated),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            MODEL_ID,
            CardFamily::World,
            MODEL_VERSION,
            "Imports an OpenStreetMap XML extract into a lane-level world: drivable, \
             pedestrian and cycle networks, junctions with connectors and conflict \
             matrices, synthesised fixed-time signals, buildings with sourced heights, \
             land-use zones and a full provenance record.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maxspeed_parsing_covers_the_forms_that_occur() {
        assert_eq!(parse_maxspeed("50"), Maxspeed::Mps(50.0 / 3.6));
        assert_eq!(parse_maxspeed(" 30 km/h "), Maxspeed::Mps(30.0 / 3.6));
        // The factor is evaluated exactly as the parser evaluates it: `v * (k / 3.6)`,
        // not `v * k / 3.6`, which differ in the last bit and would make this test a
        // platform-dependent coin toss rather than an exact check.
        assert_eq!(
            parse_maxspeed("30 mph"),
            Maxspeed::Mps(30.0 * (1.609_344 / 3.6))
        );
        assert_eq!(parse_maxspeed("walk"), Maxspeed::Mps(5.0 / 3.6));
        assert_eq!(parse_maxspeed("none"), Maxspeed::Unlimited);
        assert_eq!(parse_maxspeed("signals"), Maxspeed::Unparsable);
        assert_eq!(parse_maxspeed("RU:urban"), Maxspeed::Unparsable);
        assert_eq!(parse_maxspeed("-5"), Maxspeed::Unparsable);
        assert_eq!(parse_maxspeed(""), Maxspeed::Absent);
        // A multi-valued limit takes the first.
        assert_eq!(parse_maxspeed("50;30"), Maxspeed::Mps(50.0 / 3.6));
    }

    #[test]
    fn oneway_parsing_covers_the_documented_vocabulary() {
        assert_eq!(parse_oneway("yes"), Oneway::Forward);
        assert_eq!(parse_oneway("1"), Oneway::Forward);
        assert_eq!(parse_oneway("-1"), Oneway::Backward);
        assert_eq!(parse_oneway("reverse"), Oneway::Backward);
        assert_eq!(parse_oneway("no"), Oneway::Both);
        assert_eq!(parse_oneway("reversible"), Oneway::Reversible);
        assert_eq!(parse_oneway("sometimes"), Oneway::Unparsable);
        assert_eq!(parse_oneway(""), Oneway::Absent);
    }

    #[test]
    fn length_parsing_handles_metric_and_imperial() {
        let m = |s: &str| parse_measure(s).map(|v| v.metres);
        assert_eq!(m("12"), Some(12.0));
        assert_eq!(m("12 m"), Some(12.0));
        assert_eq!(m("12.5m"), Some(12.5));
        assert_eq!(m("350 cm"), Some(3.5));
        assert_eq!(m("10 ft"), Some(3.048));
        assert_eq!(m("12'"), Some(12.0 * 0.3048));
        assert!(
            (m("12'6\"").expect("feet and inches") - (12.0 * 0.3048 + 6.0 * 0.0254)).abs() < 1e-12
        );
        assert_eq!(m("tall"), None);
        // Multi-valued: first value, flagged.
        let multi = parse_measure("12;15").expect("a multi-valued height");
        assert_eq!(multi.metres, 12.0);
        assert!(multi.multi_valued);
        assert!(!parse_measure("12").expect("a plain height").multi_valued);
    }

    #[test]
    fn turn_lane_parsing_is_all_or_nothing() {
        let parsed = parse_turn_lanes("left|through;right|").expect("a valid value");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], vec![TurnDirection::Left]);
        assert_eq!(
            parsed[1],
            vec![TurnDirection::Straight, TurnDirection::Right]
        );
        assert!(parsed[2].is_empty(), "an empty token is unrestricted");
        assert_eq!(parse_turn_lanes("left|sideways"), None);
        // `none` and an empty token both mean unrestricted.
        assert_eq!(parse_turn_lanes("none|none"), Some(vec![vec![], vec![]]));
    }

    #[test]
    fn restriction_parsing_keeps_the_direction_and_the_turn() {
        let forbid = |turn| {
            Some(Restriction {
                kind: RestrictionKind::Forbid,
                turn,
            })
        };
        assert_eq!(
            parse_restriction("no_left_turn"),
            forbid(Some(TurnDirection::Left))
        );
        assert_eq!(
            parse_restriction("no_right_turn"),
            forbid(Some(TurnDirection::Right))
        );
        assert_eq!(
            parse_restriction("no_straight_on"),
            forbid(Some(TurnDirection::Straight))
        );
        assert_eq!(
            parse_restriction("no_u_turn"),
            forbid(Some(TurnDirection::UTurn))
        );
        // R4: `no_entry` and `no_exit` name a member set, not a turn, and an unknown
        // suffix names nothing — both still forbid the from x to product.
        assert_eq!(parse_restriction("no_entry"), forbid(None));
        assert_eq!(parse_restriction("no_something_new"), forbid(None));
        assert_eq!(
            parse_restriction("only_straight_on"),
            Some(Restriction {
                kind: RestrictionKind::Only,
                turn: Some(TurnDirection::Straight),
            })
        );
        assert_eq!(parse_restriction("give_way"), None);
    }

    #[test]
    fn lane_assignment_follows_the_documented_rule() {
        assert_eq!(lanes_for_turn(TurnDirection::Right, 3), vec![0]);
        assert_eq!(lanes_for_turn(TurnDirection::Left, 3), vec![2]);
        assert_eq!(lanes_for_turn(TurnDirection::Straight, 3), vec![0, 1, 2]);
        // A single-lane approach makes every movement from its one lane.
        assert_eq!(lanes_for_turn(TurnDirection::Left, 1), vec![0]);
        assert_eq!(target_lane(TurnDirection::Right, 2, &[2], 3), 0);
        assert_eq!(target_lane(TurnDirection::Left, 0, &[0], 3), 2);
        assert_eq!(target_lane(TurnDirection::Straight, 1, &[1], 3), 1);
        assert_eq!(target_lane(TurnDirection::Straight, 2, &[2], 2), 1);
        // A double right turn pairs off from the kerb instead of doubling up on it.
        assert_eq!(target_lane(TurnDirection::Right, 0, &[0, 1], 3), 0);
        assert_eq!(target_lane(TurnDirection::Right, 1, &[0, 1], 3), 1);
        // A double left, from the other kerb.
        assert_eq!(target_lane(TurnDirection::Left, 2, &[1, 2], 3), 2);
        assert_eq!(target_lane(TurnDirection::Left, 1, &[1, 2], 3), 1);
        // Three through lanes after a right-turn-only lane, into three departure lanes:
        // shifted right by one rather than two of them sharing the last lane.
        assert_eq!(target_lane(TurnDirection::Straight, 1, &[1, 2, 3], 3), 0);
        assert_eq!(target_lane(TurnDirection::Straight, 3, &[1, 2, 3], 3), 2);
    }

    #[test]
    fn a_tagged_turn_matches_the_geometry_it_was_meant_for() {
        assert!(turn_matches(
            TurnDirection::Straight,
            TurnDirection::SlightLeft
        ));
        assert!(turn_matches(TurnDirection::Left, TurnDirection::SlightLeft));
        assert!(!turn_matches(TurnDirection::Left, TurnDirection::Straight));
        assert!(!turn_matches(TurnDirection::Right, TurnDirection::Left));
        assert!(turn_matches(TurnDirection::UTurn, TurnDirection::UTurn));
    }

    #[test]
    fn trimming_cuts_the_requested_arc_range() {
        let line = vec![
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(10.0, 0.0),
            Vec3::new_2d(20.0, 0.0),
        ];
        let cut = trim_polyline(&line, 2.0, 18.0).expect("a valid range");
        assert_eq!(cut.first().expect("start").x, 2.0);
        assert_eq!(cut.last().expect("end").x, 18.0);
        assert!((polyline_length(&cut) - 16.0).abs() < 1e-9);
        // The interior vertex survives.
        assert_eq!(cut.len(), 3);
        assert!(trim_polyline(&line, 5.0, 5.0).is_none());
    }

    #[test]
    fn offsetting_keeps_a_constant_distance_from_a_straight_line() {
        let line = vec![Vec3::new_2d(0.0, 0.0), Vec3::new_2d(10.0, 0.0)];
        let (left, repaired) = offset_polyline(&line, 3.0);
        assert!(!repaired, "a straight line needs no repair");
        assert_eq!(left[0], Vec3::new_2d(0.0, 3.0));
        assert_eq!(left[1], Vec3::new_2d(10.0, 3.0));
        let (right, _) = offset_polyline(&line, -3.0);
        assert_eq!(right[0], Vec3::new_2d(0.0, -3.0));
        // A right angle mitres: the corner point moves out along the bisector.
        let corner = vec![
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(10.0, 0.0),
            Vec3::new_2d(10.0, 10.0),
        ];
        let (offset, repaired) = offset_polyline(&corner, -2.0);
        assert!(!repaired, "a right angle is not a tight bend");
        assert!((offset[1].x - 12.0).abs() < 1e-9, "{:?}", offset[1]);
        assert!((offset[1].y + 2.0).abs() < 1e-9, "{:?}", offset[1]);
    }
    /// R1. A bend whose radius is smaller than the lane offset used to invert: the mitre
    /// threw the inner vertex past its neighbours and the centreline crossed itself, and
    /// nothing noticed. The offset of this hairpin must come back free of self-crossings
    /// and must say that it was repaired.
    #[test]
    fn offsetting_a_bend_tighter_than_the_offset_does_not_invert() {
        // A 1 m hairpin offset by 5 to 8 m: the corner's mitre reaches past both arms.
        let hairpin = vec![
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(10.0, 0.0),
            Vec3::new_2d(11.0, 1.0),
            Vec3::new_2d(10.0, 2.0),
            Vec3::new_2d(0.0, 2.0),
        ];
        for d in [5.0, -5.0, 8.0, -8.0] {
            let (offset, _) = offset_polyline(&hairpin, d);
            assert!(
                first_self_crossing(&offset).is_none(),
                "offset by {d} still crosses itself: {offset:?}"
            );
            assert!(offset.len() >= 2, "offset by {d} kept its ends");
        }
        // The hairpin turns right, so its **left** offset is the inner one: that is the
        // side whose vertices invert, and it is reported as repaired. The outer side only
        // gets bigger and needs nothing.
        let (_, inner) = offset_polyline(&hairpin, 8.0);
        let (_, outer) = offset_polyline(&hairpin, -8.0);
        assert!(inner, "the inner side of a hairpin needs repair");
        assert!(!outer, "the outer side of a hairpin does not");
    }

    /// R1, the other half: a polyline that already crosses itself has the loop cut out at
    /// the crossing point, and its ends are untouched.
    #[test]
    fn a_self_crossing_polyline_has_its_loop_cut_out() {
        // Segment 0, (0,0)-(10,0), and segment 2, (10,10)-(5,-5), cross at (6.67, 0).
        let mut bow = vec![
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(10.0, 0.0),
            Vec3::new_2d(10.0, 10.0),
            Vec3::new_2d(5.0, -5.0),
        ];
        let (first, last) = (bow[0], bow[3]);
        assert!(first_self_crossing(&bow).is_some(), "the fixture crosses");
        assert!(cut_self_crossings(&mut bow));
        assert!(first_self_crossing(&bow).is_none(), "{bow:?}");
        assert_eq!(bow[0], first);
        assert_eq!(bow[bow.len() - 1], last);
        // Idempotent: a clean polyline is left alone.
        assert!(!cut_self_crossings(&mut bow));
    }

    /// V5. The clip cuts a way where it leaves the box, interpolates the crossing point
    /// and gives the cut end a synthetic id below every real one.
    #[test]
    fn clipping_cuts_a_way_at_the_box_and_names_the_cut() {
        let clip = ClipBox {
            min_x: 0.0,
            min_y: 0.0,
            max_x: 100.0,
            max_y: 100.0,
        };
        let nodes = vec![10, 11, 12, 13];
        let geometry = vec![
            Vec3::new_2d(-50.0, 50.0),
            Vec3::new_2d(50.0, 50.0),
            Vec3::new_2d(60.0, 50.0),
            Vec3::new_2d(200.0, 50.0),
        ];
        let mut next = 0i64;
        let (runs, cut) = clip_runs(&nodes, &geometry, &clip, &mut next);
        assert!(cut);
        assert_eq!(runs.len(), 1);
        let (ids, points) = &runs[0];
        assert_eq!(points.len(), 4, "{points:?}");
        assert_eq!(
            points[0],
            Vec3::new_2d(0.0, 50.0),
            "the entry crossing is interpolated"
        );
        assert_eq!(
            points[3],
            Vec3::new_2d(100.0, 50.0),
            "the exit crossing is interpolated"
        );
        assert!(ids[0] < 0 && ids[3] < 0, "cut ends are synthetic: {ids:?}");
        assert_eq!(&ids[1..3], &[11, 12]);
        // A way that leaves the box and comes back gives two runs.
        let zigzag = vec![
            Vec3::new_2d(10.0, 10.0),
            Vec3::new_2d(-10.0, 10.0),
            Vec3::new_2d(10.0, 20.0),
            Vec3::new_2d(20.0, 20.0),
        ];
        let (runs, cut) = clip_runs(&[1, 2, 3, 4], &zigzag, &clip, &mut next);
        assert!(cut);
        assert_eq!(runs.len(), 2, "{runs:?}");
        // A way wholly inside is returned untouched and reports no cut.
        let inside = vec![Vec3::new_2d(10.0, 10.0), Vec3::new_2d(20.0, 20.0)];
        let (runs, cut) = clip_runs(&[1, 2], &inside, &clip, &mut next);
        assert!(!cut);
        assert_eq!(runs, vec![(vec![1, 2], inside)]);
    }

    /// V4/W1. Both presets exist, both are named, and their motor speeds differ in the
    /// way the sources say: German design speeds against the New York City legal
    /// defaults.
    #[test]
    fn the_speed_presets_are_named_and_sourced() {
        for preset in HighwayPreset::ALL {
            assert_eq!(HighwayPreset::parse(preset.label()), Some(preset));
            assert!(
                !preset.source().is_empty(),
                "{} has no source",
                preset.label()
            );
            for row in preset.table() {
                assert!(
                    !row.speed_source.is_empty(),
                    "{} row {} has no speed source",
                    preset.label(),
                    row.key
                );
                assert!(row.speed_mps > 0.0);
            }
        }
        assert_eq!(HighwayPreset::parse("nonesuch"), None);
        let german = HighwayPreset::SumoGerman.row("secondary").expect("a row");
        let nyc = HighwayPreset::UrbanUsNyc.row("secondary").expect("a row");
        assert_eq!(german.speed_mps, 27.78, "100 km/h, SUMO's design speed");
        assert_eq!(nyc.speed_mps, 11.176, "25 mph, the NYC citywide default");
        // The lane count is geometry, not jurisdiction, so it does not change.
        assert_eq!(german.lanes, nyc.lanes);
        // Walking pace is not a legal limit and is the same in both.
        assert_eq!(
            HighwayPreset::SumoGerman
                .row("steps")
                .expect("a row")
                .speed_mps,
            HighwayPreset::UrbanUsNyc
                .row("steps")
                .expect("a row")
                .speed_mps
        );
    }

    /// V6/W2. The width rule reads the tags first, then the class, and only then the
    /// caller's global override.
    #[test]
    fn lane_width_prefers_the_tag_then_the_class() {
        let mut report = ImportReport::default();
        let tags = |pairs: &[(&str, &str)]| {
            let mut t = Tags::default();
            for (k, v) in pairs {
                t.insert((*k).to_string(), (*v).to_string());
            }
            t
        };
        let options = OsmOptions::default();
        // A class default, not one global constant: an avenue and an alley differ.
        let (avenue, source) = motor_lane_width(
            &Tags::default(),
            RoadClass::Primary,
            4,
            1,
            &options,
            &mut report,
        );
        assert_eq!(source, WidthSource::Class);
        assert_eq!(avenue, 3.353);
        let (alley, _) = motor_lane_width(
            &Tags::default(),
            RoadClass::Service,
            1,
            1,
            &options,
            &mut report,
        );
        assert_eq!(alley, 2.743);
        assert!(avenue > alley, "an avenue lane is wider than an alley lane");
        // `width` is the whole carriageway, so it is divided by the lane count.
        let (tagged, source) = motor_lane_width(
            &tags(&[("width", "13")]),
            RoadClass::Primary,
            4,
            1,
            &options,
            &mut report,
        );
        assert_eq!(source, WidthSource::WayTag);
        assert_eq!(tagged, 3.25);
        // `width:lanes` gives one width per lane; the model holds one, so the mean.
        let (per_lane, source) = motor_lane_width(
            &tags(&[("width:lanes", "3|3.5|3.5")]),
            RoadClass::Primary,
            3,
            1,
            &options,
            &mut report,
        );
        assert_eq!(source, WidthSource::PerLaneTag);
        assert!((per_lane - 10.0 / 3.0).abs() < 1e-12, "{per_lane}");
        // An implausible tag is counted and ignored.
        let before = report.anomaly(Anomaly::UnparsableHeight);
        let (fallback, source) = motor_lane_width(
            &tags(&[("width", "300")]),
            RoadClass::Residential,
            1,
            1,
            &options,
            &mut report,
        );
        assert_eq!(source, WidthSource::Class);
        assert_eq!(fallback, 3.048);
        assert_eq!(report.anomaly(Anomaly::UnparsableHeight), before + 1);
        // The global option is the last resort, and a tag still beats it.
        let forced = OsmOptions {
            lane_width_m: Some(4.25),
            ..OsmOptions::default()
        };
        let (overridden, source) = motor_lane_width(
            &Tags::default(),
            RoadClass::Primary,
            2,
            1,
            &forced,
            &mut report,
        );
        assert_eq!(source, WidthSource::Option);
        assert_eq!(overridden, 4.25);
        let (still_tagged, source) = motor_lane_width(
            &tags(&[("width", "6")]),
            RoadClass::Primary,
            2,
            1,
            &forced,
            &mut report,
        );
        assert_eq!(source, WidthSource::WayTag);
        assert_eq!(still_tagged, 3.0);
    }

    /// V1, V2/W4. The building classifier separates an outline from a part and refuses
    /// to make an obstacle of anything the tags put below ground.
    #[test]
    fn the_building_classifier_separates_outlines_parts_and_basements() {
        let tags = |pairs: &[(&str, &str)]| {
            let mut t = Tags::default();
            for (k, v) in pairs {
                t.insert((*k).to_string(), (*v).to_string());
            }
            t
        };
        assert_eq!(
            building_role(&tags(&[("building", "yes")])),
            Some(BuildingRole::Outline)
        );
        assert_eq!(
            building_role(&tags(&[("building:part", "yes")])),
            Some(BuildingRole::Part)
        );
        assert_eq!(building_role(&tags(&[("building", "no")])), None);
        assert_eq!(building_role(&Tags::default()), None);
        // The three phantom Midtown obstacles, by their real tags (V2).
        for basement in [
            vec![
                ("building", "train_station"),
                ("location", "underground"),
                ("layer", "-1"),
                ("underground", "yes"),
                ("railway", "station"),
            ],
            vec![("building", "yes"), ("layer", "-2")],
            vec![("building", "roof"), ("underground", "yes")],
            vec![("building:part", "yes"), ("railway", "station")],
            vec![("building:part", "yes"), ("public_transport", "station")],
        ] {
            assert_eq!(
                building_role(&tags(&basement)),
                Some(BuildingRole::Subsurface),
                "{basement:?}"
            );
        }
        // A station that really has a head house above ground is still a building.
        assert_eq!(
            building_role(&tags(&[
                ("building", "train_station"),
                ("railway", "station")
            ])),
            Some(BuildingRole::Outline)
        );
    }

    #[test]
    fn the_hull_of_a_square_is_that_square_closed_and_counter_clockwise() {
        let points = [
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(1.0, 0.0),
            Vec3::new_2d(1.0, 1.0),
            Vec3::new_2d(0.0, 1.0),
            Vec3::new_2d(0.5, 0.5),
        ];
        let hull = convex_hull(&points);
        assert_eq!(hull.len(), 5, "four corners plus the repeated first");
        assert_eq!(hull.first(), hull.last());
        assert!(crate::model::ring_signed_area_2x(&hull) > 0.0);
        // Fewer than three distinct points has no area.
        assert!(convex_hull(&points[..2]).is_empty());
        let collinear = [
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(1.0, 0.0),
            Vec3::new_2d(2.0, 0.0),
        ];
        assert!(convex_hull(&collinear).is_empty());
    }

    #[test]
    fn rings_are_chained_from_their_members_in_way_order() {
        let mut report = ImportReport::default();
        // Three fragments of one ring, listed out of order and one of them reversed.
        let pool = vec![(3, vec![7, 1]), (1, vec![1, 3]), (2, vec![3, 5, 7])];
        let rings = chain_rings(pool, 99, &mut report);
        assert_eq!(rings, vec![vec![1, 3, 5, 7, 1]]);
        assert_eq!(report.anomaly(Anomaly::UnclosedRing), 0);

        // A fragment that cannot be closed is dropped and counted.
        let mut report = ImportReport::default();
        let rings = chain_rings(vec![(1, vec![1, 2, 3])], 99, &mut report);
        assert!(rings.is_empty());
        assert_eq!(report.anomaly(Anomaly::UnclosedRing), 1);
    }

    #[test]
    fn phase_groups_split_a_junction_into_two_axes() {
        let east = 0.0;
        let north = core::f64::consts::FRAC_PI_2;
        let west = core::f64::consts::PI;
        assert_eq!(phase_group(east, east), 0);
        assert_eq!(
            phase_group(west, east),
            0,
            "opposing approaches share a phase"
        );
        assert_eq!(phase_group(north, east), 1);
        assert_eq!(phase_group(-north, east), 1);
    }

    #[test]
    fn the_origin_grid_rounds_downwards() {
        // 40.744000_05 must floor to 40.7440000, never up to 40.7440001.
        let v = 40.744_000_05;
        let floored = floor_to_grid(v, Q_DEGREES);
        assert!(floored <= v, "{floored} > {v}");
        assert!(v - floored < Q_DEGREES);
        assert!(crate::quant::is_on_grid(floored, Q_DEGREES));
        // An exact grid point is left alone.
        assert_eq!(
            floor_to_grid(40.744, Q_DEGREES),
            quantise(40.744, Q_DEGREES)
        );
    }
}
