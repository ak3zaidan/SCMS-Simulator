//! `world/source/procedural-grid` — the procedural grid generator (04-models.md §1.2).
//!
//! A rectangular lattice of junctions joined by two-way streets, with correct junction
//! internals, connections, conflict matrices, optional fixed-time signals, optional
//! crossings, optional block buildings and optional RSU sites. It is deterministic from
//! its parameters alone: no random numbers are drawn, so the same parameters give the
//! same world, byte for byte, on every platform (conformance item W5).
//!
//! It is also what most of this crate's tests build on, which is deliberate: the
//! generator exercises every part of the model — internal lanes, conflict matrices,
//! signal plans, rings, sites — so a change that breaks the model breaks these tests
//! before it reaches an importer.
//!
//! # Geometry
//!
//! Junction `(i, j)` sits at `(half + i · block_x, half + j · block_y)`, where
//! `half = lanes_per_direction · lane_width` is the half-width of the carriageway. The
//! world origin is therefore the bounding box's south-west corner and every coordinate is
//! non-negative, as D6 requires.
//!
//! Right-hand traffic: an east-bound lane lies south of its street's centreline, a
//! north-bound lane east of its street's centreline, and lane index 0 is the rightmost in
//! the direction of travel (docs/protocol/vwp-v1.md §4.3).
//!
//! # Id assignment (deterministic, documented)
//!
//! 1. **Junctions**, row-major: `id = row · cols + col`.
//! 2. **Street edges**: every east-west street first, row by row, west to east, each
//!    junction pair contributing its east-bound edge then its west-bound edge; then every
//!    north-south street, column by column, south to north, north-bound then south-bound.
//! 3. **Street lanes**: as their edge is created, in index order, `0` = rightmost.
//! 4. **Internal edges**: one per junction, in junction id order, after every street edge.
//! 5. **Internal lanes**: one per movement, in movement order (below), after every street
//!    lane.
//! 6. **Movements** at a junction: approaches in the fixed direction order east, north,
//!    west, south — the direction of *travel into* the junction — then by approach lane
//!    index from the rightmost, then right turn, straight on, left turn.
//! 7. **Crossings**: by junction, then by the same direction order.
//! 8. **Buildings**: block `(i, j)` row-major. **Sites**: by junction id. **Signal
//!    plans**: by junction id, among signalised junctions only.
//!
//! # The other generators
//!
//! 04-models.md §1.2 lists five procedural topologies. Three of them are here:
//!
//! | Model id | Module | Geometry |
//! |---|---|---|
//! | `world/source/procedural-grid` | this module | a rectangular lattice |
//! | `world/source/procedural-radial` | [`radial`] | the spider: spokes and ring roads |
//! | `world/source/procedural-random` | [`random`] | random growth from one junction |
//!
//! The lattice computes its own geometry, because every junction in it is a right-angled
//! crossroads. The other two cannot, so they share [`graph`], which turns a list of node
//! positions and undirected streets — the legacy `CustomNetwork` input — into the full
//! lane-level model with junction areas, connectors and conflict matrices.
//!
//! `world/source/procedural-suburban` and `world/source/procedural-highway` are not
//! implemented; 04-models.md §1.2 records both of their parameter sets as
//! `TODO: calibrate`, so there is nothing to build them from yet.

pub mod graph;
pub mod radial;
pub mod random;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{BuildingId, EdgeId, JunctionId, LaneId, SignalId};

use crate::error::{Result, WorldError};
use crate::model::{
    Building, ClassMask, ConflictMatrix, Connection, Crossing, CrossingId, Edge, EnvClass,
    GeoOrigin, HeightSource, Junction, JunctionControl, LanduseClass, LanduseZone, Lane, LaneKind,
    LayerLicence, MaterialClass, RoadClass, RoadNetwork, SignalHead, SignalHeadKind, SignalPhase,
    SignalPlan, SignalState, Site, SiteId, SiteKind, SymbolTable, Transformation, TurnDirection,
    World, WorldProvenance, WorldSourceKind, ZoneId,
};
use crate::{ImportOptions, WorldSource, WorldSourceSpec};

/// The model id of this generator (04-models.md §1.2).
pub const MODEL_ID: &str = "world/source/procedural-grid";

/// The generator's own version, as the model card reports it.
pub const MODEL_VERSION: &str = "1.0.0";

/// One of the four cardinal directions of travel, in the world's ENU frame.
///
/// The enum order — east, north, west, south, i.e. counter-clockwise from east — is the
/// order movements are generated in, so it is part of the documented id assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dir4 {
    /// Travelling east, `+x`.
    East,
    /// Travelling north, `+y`.
    North,
    /// Travelling west, `-x`.
    West,
    /// Travelling south, `-y`.
    South,
}

impl Dir4 {
    /// All four, in generation order.
    pub const ALL: [Dir4; 4] = [Dir4::East, Dir4::North, Dir4::West, Dir4::South];

    /// The unit vector of travel.
    pub const fn forward(self) -> (f64, f64) {
        match self {
            Dir4::East => (1.0, 0.0),
            Dir4::North => (0.0, 1.0),
            Dir4::West => (-1.0, 0.0),
            Dir4::South => (0.0, -1.0),
        }
    }

    /// The unit vector to the driver's right: `forward` rotated 90° clockwise.
    pub const fn right_vector(self) -> (f64, f64) {
        let (x, y) = self.forward();
        (y, -x)
    }

    /// The direction after turning left (counter-clockwise).
    pub const fn left(self) -> Dir4 {
        match self {
            Dir4::East => Dir4::North,
            Dir4::North => Dir4::West,
            Dir4::West => Dir4::South,
            Dir4::South => Dir4::East,
        }
    }

    /// The direction after turning right (clockwise).
    pub const fn right(self) -> Dir4 {
        match self {
            Dir4::East => Dir4::South,
            Dir4::South => Dir4::West,
            Dir4::West => Dir4::North,
            Dir4::North => Dir4::East,
        }
    }

    /// The opposite direction.
    pub const fn opposite(self) -> Dir4 {
        self.left().left()
    }

    /// The step in grid indices this direction takes.
    pub const fn step(self) -> (i64, i64) {
        match self {
            Dir4::East => (1, 0),
            Dir4::North => (0, 1),
            Dir4::West => (-1, 0),
            Dir4::South => (0, -1),
        }
    }

    /// The signal phase group: `0` for the east-west street, `1` for the north-south one.
    pub const fn phase_group(self) -> u16 {
        match self {
            Dir4::East | Dir4::West => 0,
            Dir4::North | Dir4::South => 1,
        }
    }

    /// A stable index, used only as a `BTreeMap` key.
    const fn code(self) -> u8 {
        match self {
            Dir4::East => 0,
            Dir4::North => 1,
            Dir4::West => 2,
            Dir4::South => 3,
        }
    }
}

/// The parameters of `world/source/procedural-grid`.
///
/// Every field is a model-card parameter ([`card`]); the two presets are the ones
/// 04-models.md §1.2 records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GridParams {
    /// Junctions along `x` (the legacy `grid_w`). At least 2.
    pub cols: u32,
    /// Junctions along `y` (the legacy `grid_h`). At least 2.
    pub rows: u32,
    /// Junction spacing along `x`, metres.
    pub block_x_m: f64,
    /// Junction spacing along `y`, metres.
    pub block_y_m: f64,
    /// Lanes per direction of travel.
    pub lanes_per_direction: u32,
    /// Lane width, metres.
    pub lane_width_m: f64,
    /// Sidewalk width on each side, metres. Buildings are inset by it; no sidewalk lanes
    /// are generated.
    pub sidewalk_m: f64,
    /// Speed limit on every street, m/s.
    pub speed_limit_mps: f64,
    /// Whether every junction gets a fixed-time signal plan.
    pub signalised: bool,
    /// Signal cycle length, seconds.
    pub cycle_s: f64,
    /// Amber time at the end of each green, seconds.
    pub amber_s: f64,
    /// Height of a signal lantern above the road, metres.
    pub signal_head_height_m: f64,
    /// Whether to generate a crossing across every street arm at every junction.
    pub crossings: bool,
    /// Crossing width, metres.
    pub crossing_width_m: f64,
    /// Whether to fill each block with one building.
    pub block_buildings: bool,
    /// Block building height, metres.
    pub building_height_m: f64,
    /// Whether to put an RSU site at every junction.
    pub rsu_at_junctions: bool,
    /// RSU antenna height above ground, metres.
    pub rsu_antenna_height_m: f64,
    /// RSU antenna gain, dBi.
    pub rsu_antenna_gain_dbi: f64,
}

impl Default for GridParams {
    /// The `legacy` preset of 04-models.md §1.2.
    fn default() -> Self {
        GridParams::legacy()
    }
}

impl GridParams {
    /// The `legacy` preset: the legacy engine's own grid defaults
    /// (`grid_w` 6, `grid_h` 6, `grid_block_m` 120, `n_lanes` 1, `lane_width_m` 3.5;
    /// 04-models.md §1.2, from `run.py` L412-424).
    pub fn legacy() -> Self {
        Self {
            cols: 6,
            rows: 6,
            block_x_m: 120.0,
            block_y_m: 120.0,
            lanes_per_direction: 1,
            lane_width_m: 3.5,
            sidewalk_m: 0.0,
            speed_limit_mps: 13.89,
            signalised: false,
            cycle_s: 60.0,
            amber_s: 3.0,
            signal_head_height_m: 5.0,
            crossings: false,
            crossing_width_m: 4.0,
            block_buildings: false,
            building_height_m: 20.0,
            rsu_at_junctions: false,
            rsu_antenna_height_m: 6.0,
            rsu_antenna_gain_dbi: 5.0,
        }
    }

    /// The `tr36885-urban` preset of 04-models.md §1.2: block 433 m × 250 m, 2 lanes per
    /// direction, lane width 3.5 m, sidewalk 3 m, minimum area 1 299 m × 750 m
    /// (TR 36.885 Table A.1.2-1), street width 20 m (TR 37.885 Annex A Fig. A-2).
    ///
    /// The minimum area fixes the lattice: 4 × 4 junctions give 1 299 m × 750 m of
    /// street. The 20 m street width is exactly the 14 m of carriageway that 2 × 2 lanes
    /// of 3.5 m make, plus a 3 m sidewalk on each side, so the preset reproduces the
    /// reference layout rather than approximating it.
    pub fn tr36885_urban() -> Self {
        Self {
            cols: 4,
            rows: 4,
            block_x_m: 433.0,
            block_y_m: 250.0,
            lanes_per_direction: 2,
            lane_width_m: 3.5,
            sidewalk_m: 3.0,
            speed_limit_mps: 13.89,
            signalised: true,
            cycle_s: 60.0,
            amber_s: 3.0,
            signal_head_height_m: 5.0,
            crossings: true,
            crossing_width_m: 4.0,
            block_buildings: true,
            building_height_m: 20.0,
            rsu_at_junctions: false,
            rsu_antenna_height_m: 6.0,
            rsu_antenna_gain_dbi: 5.0,
        }
    }

    /// Sets both block dimensions.
    #[must_use]
    pub fn with_block_m(mut self, block_m: f64) -> Self {
        self.block_x_m = block_m;
        self.block_y_m = block_m;
        self
    }

    /// Sets the lattice size.
    #[must_use]
    pub fn with_size(mut self, cols: u32, rows: u32) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Turns signals on or off.
    #[must_use]
    pub fn with_signals(mut self, signalised: bool) -> Self {
        self.signalised = signalised;
        self
    }

    /// Half the carriageway width, metres: the junction's half-extent.
    pub fn half_width_m(&self) -> f64 {
        f64::from(self.lanes_per_direction) * self.lane_width_m
    }

    /// Checks that the parameters describe a world that can exist.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter, for a lattice smaller than
    /// 2 × 2, a non-positive width or block, a block too short to hold a lane between two
    /// junction areas, or a signal cycle that the amber times do not fit into.
    pub fn validate(&self) -> Result<()> {
        let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
            parameter: parameter.to_string(),
            problem,
        };
        if self.cols < 2 {
            return Err(bad(
                "cols",
                format!("{} junctions, need at least 2", self.cols),
            ));
        }
        if self.rows < 2 {
            return Err(bad(
                "rows",
                format!("{} junctions, need at least 2", self.rows),
            ));
        }
        if self.lanes_per_direction == 0 {
            return Err(bad(
                "lanes_per_direction",
                "a street needs at least one lane per direction".to_string(),
            ));
        }
        if !(self.lane_width_m.is_finite() && self.lane_width_m > 0.0) {
            return Err(bad(
                "lane_width_m",
                format!("{} m is not a positive width", self.lane_width_m),
            ));
        }
        if self.sidewalk_m < 0.0 {
            return Err(bad(
                "sidewalk_m",
                format!("{} m is negative", self.sidewalk_m),
            ));
        }
        let half = self.half_width_m();
        for (name, block) in [("block_x_m", self.block_x_m), ("block_y_m", self.block_y_m)] {
            if block <= 2.0 * half + 1.0 {
                return Err(bad(
                    name,
                    format!(
                        "{block} m leaves no room between two {} m junction areas; \
                         a block must exceed 2 × half-width + 1 m = {} m",
                        2.0 * half,
                        2.0 * half + 1.0
                    ),
                ));
            }
        }
        if !(self.speed_limit_mps.is_finite() && self.speed_limit_mps > 0.0) {
            return Err(bad(
                "speed_limit_mps",
                format!("{} m/s is not a positive speed", self.speed_limit_mps),
            ));
        }
        if self.signalised {
            if !(self.amber_s.is_finite() && self.amber_s > 0.0) {
                return Err(bad(
                    "amber_s",
                    format!("{} s is not a positive amber time", self.amber_s),
                ));
            }
            if self.cycle_s <= 2.0 * self.amber_s {
                return Err(bad(
                    "cycle_s",
                    format!(
                        "a {} s cycle cannot hold two {} s ambers and two greens",
                        self.cycle_s, self.amber_s
                    ),
                ));
            }
        }
        if self.crossings && !(self.crossing_width_m.is_finite() && self.crossing_width_m > 0.0) {
            return Err(bad(
                "crossing_width_m",
                format!("{} m is not a positive width", self.crossing_width_m),
            ));
        }
        if self.block_buildings
            && !(self.building_height_m.is_finite() && self.building_height_m > 0.0)
        {
            return Err(bad(
                "building_height_m",
                format!("{} m is not a positive height", self.building_height_m),
            ));
        }
        Ok(())
    }
}

/// The lattice's metric layout: everything the geometry needs, computed once.
#[derive(Debug, Clone, Copy)]
struct Layout {
    cols: u32,
    rows: u32,
    block_x_m: f64,
    block_y_m: f64,
    lanes: u32,
    width_m: f64,
    half_m: f64,
}

impl Layout {
    fn new(p: &GridParams) -> Self {
        Self {
            cols: p.cols,
            rows: p.rows,
            block_x_m: p.block_x_m,
            block_y_m: p.block_y_m,
            lanes: p.lanes_per_direction,
            width_m: p.lane_width_m,
            half_m: p.half_width_m(),
        }
    }

    /// The junction id of lattice cell `(col, row)`, row-major.
    fn junction_id(&self, col: u32, row: u32) -> JunctionId {
        JunctionId::new(row * self.cols + col)
    }

    /// The lattice cell of a junction id.
    fn cell(&self, id: JunctionId) -> (u32, u32) {
        (id.index() % self.cols, id.index() / self.cols)
    }

    /// The centre of a junction, world-local metres.
    fn junction_xy(&self, col: u32, row: u32) -> (f64, f64) {
        (
            self.half_m + f64::from(col) * self.block_x_m,
            self.half_m + f64::from(row) * self.block_y_m,
        )
    }

    /// The neighbour of `(col, row)` in direction `d`, if it is on the lattice.
    fn neighbour(&self, col: u32, row: u32, d: Dir4) -> Option<(u32, u32)> {
        let (dx, dy) = d.step();
        let nc = i64::from(col) + dx;
        let nr = i64::from(row) + dy;
        if nc < 0 || nr < 0 || nc >= i64::from(self.cols) || nr >= i64::from(self.rows) {
            return None;
        }
        Some((nc as u32, nr as u32))
    }

    /// The lateral offset of lane `k` from its street's centreline, to the right of
    /// travel: lane 0 is the rightmost, so it is the farthest from the centreline.
    fn lane_offset_m(&self, k: u32) -> f64 {
        (f64::from(self.lanes) - 0.5 - f64::from(k)) * self.width_m
    }

    /// The point where lane `k` of the departure in direction `d` leaves junction
    /// `(col, row)`.
    fn departure_point(&self, col: u32, row: u32, d: Dir4, k: u32) -> Vec3 {
        let (cx, cy) = self.junction_xy(col, row);
        let (fx, fy) = d.forward();
        let (rx, ry) = d.right_vector();
        let off = self.lane_offset_m(k);
        Vec3::new_2d(
            cx + fx * self.half_m + rx * off,
            cy + fy * self.half_m + ry * off,
        )
    }

    /// The point where lane `k` of the approach travelling `d` reaches junction
    /// `(col, row)`.
    fn approach_point(&self, col: u32, row: u32, d: Dir4, k: u32) -> Vec3 {
        let (cx, cy) = self.junction_xy(col, row);
        let (fx, fy) = d.forward();
        let (rx, ry) = d.right_vector();
        let off = self.lane_offset_m(k);
        Vec3::new_2d(
            cx - fx * self.half_m + rx * off,
            cy - fy * self.half_m + ry * off,
        )
    }

    /// The whole world's extent.
    fn extent(&self) -> (f64, f64) {
        (
            2.0 * self.half_m + f64::from(self.cols - 1) * self.block_x_m,
            2.0 * self.half_m + f64::from(self.rows - 1) * self.block_y_m,
        )
    }
}

/// One movement through a junction, while the generator is building it.
#[derive(Debug, Clone, Copy)]
struct Movement {
    approach: Dir4,
    from_lane: LaneId,
    to_lane: LaneId,
    internal: LaneId,
    turn: TurnDirection,
}

/// Samples a quadratic Bézier through `(start, control, end)` at `points` points.
///
/// Polynomial arithmetic only — no transcendental — so a turn's geometry is bit-identical
/// on every platform. Seven points is enough that the polyline's sagitta is under a
/// centimetre for an urban turning radius, and few enough to keep the lane graph small.
fn bezier(start: Vec3, control: Vec3, end: Vec3, points: usize) -> Vec<Vec3> {
    let n = points.max(2);
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            let u = 1.0 - t;
            Vec3::new(
                u * u * start.x + 2.0 * u * t * control.x + t * t * end.x,
                u * u * start.y + 2.0 * u * t * control.y + t * t * end.y,
                u * u * start.z + 2.0 * u * t * control.z + t * t * end.z,
            )
        })
        .collect()
}

/// The control point of a 90° turn: where the two tangent lines meet.
fn turn_control(start: Vec3, end: Vec3, from: Dir4) -> Vec3 {
    let (fx, fy) = from.forward();
    let along = (end.x - start.x) * fx + (end.y - start.y) * fy;
    Vec3::new(start.x + fx * along, start.y + fy * along, start.z)
}

/// Builds a grid world (04-models.md §1.2, model id [`MODEL_ID`]).
///
/// Deterministic from `params` alone: no random draws, no wall clock, no hash iteration.
/// `opts` contributes the import date and the index sizing, neither of which touches the
/// geometry or the content hash.
///
/// # Errors
///
/// Whatever [`GridParams::validate`] rejects, or — if a parameter combination produced
/// geometry the model refuses (a degenerate lane, say) — the model's own error.
pub fn grid(params: &GridParams, opts: &ImportOptions) -> Result<World> {
    params.validate()?;
    let layout = Layout::new(params);
    let n = layout.lanes;
    let mut symbols = SymbolTable::new();

    // --- Junctions (id order: row-major) -----------------------------------------
    let mut junctions: Vec<Junction> = Vec::with_capacity((layout.cols * layout.rows) as usize);
    for row in 0..layout.rows {
        for col in 0..layout.cols {
            let (cx, cy) = layout.junction_xy(col, row);
            let h = layout.half_m;
            junctions.push(Junction {
                id: layout.junction_id(col, row),
                position: Vec3::new_2d(cx, cy),
                shape: vec![
                    Vec3::new_2d(cx - h, cy - h),
                    Vec3::new_2d(cx + h, cy - h),
                    Vec3::new_2d(cx + h, cy + h),
                    Vec3::new_2d(cx - h, cy + h),
                    Vec3::new_2d(cx - h, cy - h),
                ],
                incoming: Vec::new(),
                outgoing: Vec::new(),
                internal: Vec::new(),
                control: JunctionControl::Priority,
                conflicts: ConflictMatrix::new(0),
                name: None,
            });
        }
    }

    // --- Street edges and their lanes ---------------------------------------------
    let mut edges: Vec<Edge> = Vec::new();
    let mut lanes: Vec<Lane> = Vec::new();
    // (junction, direction) → the edge arriving there travelling that way, and the edge
    // leaving there travelling that way. `BTreeMap`, so iteration and lookup are ordered.
    let mut incoming_edge: BTreeMap<(u32, u8), EdgeId> = BTreeMap::new();
    let mut outgoing_edge: BTreeMap<(u32, u8), EdgeId> = BTreeMap::new();

    let add_street_edge = |from: (u32, u32),
                           to: (u32, u32),
                           d: Dir4,
                           name: Option<crate::model::SymbolId>,
                           edges: &mut Vec<Edge>,
                           lanes: &mut Vec<Lane>,
                           junctions: &mut Vec<Junction>,
                           incoming_edge: &mut BTreeMap<(u32, u8), EdgeId>,
                           outgoing_edge: &mut BTreeMap<(u32, u8), EdgeId>|
     -> Result<()> {
        let edge_id = EdgeId::new(edges.len() as u32);
        let from_j = layout.junction_id(from.0, from.1);
        let to_j = layout.junction_id(to.0, to.1);
        let mut lane_ids = Vec::with_capacity(n as usize);
        for k in 0..n {
            let lane_id = LaneId::new(lanes.len() as u32);
            let start = layout.departure_point(from.0, from.1, d, k);
            let end = layout.approach_point(to.0, to.1, d, k);
            lanes.push(Lane::new(
                lane_id,
                edge_id,
                None,
                u8::try_from(k).unwrap_or(u8::MAX),
                LaneKind::Driving,
                vec![start, end],
                layout.width_m,
                params.speed_limit_mps,
                ClassMask::MOTOR_TRAFFIC,
            )?);
            lane_ids.push(lane_id);
            junctions[to_j.as_usize()].incoming.push(lane_id);
            junctions[from_j.as_usize()].outgoing.push(lane_id);
        }
        edges.push(Edge {
            id: edge_id,
            from: from_j,
            to: to_j,
            lanes: lane_ids,
            name,
            road_class: RoadClass::Residential,
        });
        incoming_edge.insert((to_j.index(), d.code()), edge_id);
        outgoing_edge.insert((from_j.index(), d.code()), edge_id);
        Ok(())
    };

    for row in 0..layout.rows {
        let name = symbols.intern_optional(&format!("Street {row}"));
        for col in 0..layout.cols - 1 {
            add_street_edge(
                (col, row),
                (col + 1, row),
                Dir4::East,
                name,
                &mut edges,
                &mut lanes,
                &mut junctions,
                &mut incoming_edge,
                &mut outgoing_edge,
            )?;
            add_street_edge(
                (col + 1, row),
                (col, row),
                Dir4::West,
                name,
                &mut edges,
                &mut lanes,
                &mut junctions,
                &mut incoming_edge,
                &mut outgoing_edge,
            )?;
        }
    }
    for col in 0..layout.cols {
        let name = symbols.intern_optional(&format!("Avenue {col}"));
        for row in 0..layout.rows - 1 {
            add_street_edge(
                (col, row),
                (col, row + 1),
                Dir4::North,
                name,
                &mut edges,
                &mut lanes,
                &mut junctions,
                &mut incoming_edge,
                &mut outgoing_edge,
            )?;
            add_street_edge(
                (col, row + 1),
                (col, row),
                Dir4::South,
                name,
                &mut edges,
                &mut lanes,
                &mut junctions,
                &mut incoming_edge,
                &mut outgoing_edge,
            )?;
        }
    }

    // --- Internal edges, internal lanes, connections, conflicts --------------------
    let mut connections: Vec<Connection> = Vec::new();
    let mut movements_by_junction: Vec<Vec<Movement>> = Vec::with_capacity(junctions.len());

    for jid in 0..junctions.len() as u32 {
        let junction = JunctionId::new(jid);
        let (col, row) = layout.cell(junction);
        let internal_edge = EdgeId::new(edges.len() as u32);
        let mut internal_lane_ids = Vec::new();
        let mut movements: Vec<Movement> = Vec::new();

        for approach in Dir4::ALL {
            // The approach exists when a street arrives here travelling `approach`.
            let Some(&in_edge) = incoming_edge.get(&(jid, approach.code())) else {
                continue;
            };
            for k in 0..n {
                let from_lane = edges[in_edge.as_usize()].lanes[k as usize];
                // Right turn from the rightmost lane, straight on from every lane, left
                // turn from the leftmost: the documented movement order.
                let mut wanted: Vec<(Dir4, u32, TurnDirection)> = Vec::new();
                if k == 0 {
                    wanted.push((approach.right(), 0, TurnDirection::Right));
                }
                wanted.push((approach, k, TurnDirection::Straight));
                if k == n - 1 {
                    wanted.push((approach.left(), n - 1, TurnDirection::Left));
                }
                for (out_dir, out_k, turn) in wanted {
                    let Some(&out_edge) = outgoing_edge.get(&(jid, out_dir.code())) else {
                        continue;
                    };
                    let to_lane = edges[out_edge.as_usize()].lanes[out_k as usize];
                    let start = layout.approach_point(col, row, approach, k);
                    let end = layout.departure_point(col, row, out_dir, out_k);
                    let geometry = if turn == TurnDirection::Straight {
                        vec![start, end]
                    } else {
                        bezier(start, turn_control(start, end, approach), end, 7)
                    };
                    let internal = LaneId::new(lanes.len() as u32);
                    lanes.push(Lane::new(
                        internal,
                        internal_edge,
                        Some(junction),
                        u8::try_from(internal_lane_ids.len()).unwrap_or(u8::MAX),
                        LaneKind::Internal,
                        geometry,
                        layout.width_m,
                        params.speed_limit_mps,
                        ClassMask::MOTOR_TRAFFIC,
                    )?);
                    internal_lane_ids.push(internal);
                    movements.push(Movement {
                        approach,
                        from_lane,
                        to_lane,
                        internal,
                        turn,
                    });
                    // The abstract hop, carrying its connector, and the concrete second
                    // half: a router sees the movement, a driver follows the geometry.
                    connections.push(Connection {
                        from_lane,
                        to_lane,
                        via: Some(internal),
                        direction: turn,
                        permitted: true,
                    });
                    connections.push(Connection {
                        from_lane: internal,
                        to_lane,
                        via: None,
                        direction: turn,
                        permitted: true,
                    });
                }
            }
        }

        // A junction with no movement at all (impossible on a 2 x 2 or bigger lattice,
        // but cheap to guard) gets no internal edge: an edge with no lanes is invalid.
        if !internal_lane_ids.is_empty() {
            edges.push(Edge {
                id: internal_edge,
                from: junction,
                to: junction,
                lanes: internal_lane_ids.clone(),
                name: None,
                road_class: RoadClass::Internal,
            });
        }
        junctions[jid as usize].internal = internal_lane_ids;
        junctions[jid as usize].conflicts = conflict_matrix(&movements, &lanes);
        movements_by_junction.push(movements);
    }

    // --- Signal plans ---------------------------------------------------------------
    let mut signals: Vec<SignalPlan> = Vec::new();
    if params.signalised {
        let green_s = (params.cycle_s - 2.0 * params.amber_s) / 2.0;
        for jid in 0..junctions.len() as u32 {
            let movements = &movements_by_junction[jid as usize];
            if movements.is_empty() {
                continue;
            }
            let plan_id = SignalId::new(signals.len() as u32);
            let controlled: Vec<LaneId> = movements.iter().map(|m| m.internal).collect();
            let phase = |group: u16, amber: bool, duration_s: f64| SignalPhase {
                duration_s,
                states: movements
                    .iter()
                    .map(|m| {
                        if m.approach.phase_group() != group {
                            SignalState::Red
                        } else if amber {
                            SignalState::Amber
                        } else if m.turn.crosses_opposing_traffic() {
                            // A permissive green: the driver may go, giving way to the
                            // conflicting movements the junction's matrix names.
                            SignalState::GreenYield
                        } else {
                            SignalState::Green
                        }
                    })
                    .collect(),
                name: None,
            };
            let mut heads: Vec<SignalHead> = Vec::new();
            let mut seen: Vec<LaneId> = Vec::new();
            for m in movements {
                if seen.contains(&m.from_lane) {
                    continue;
                }
                seen.push(m.from_lane);
                let end = lanes[m.from_lane.as_usize()].end();
                heads.push(SignalHead {
                    lane: m.from_lane,
                    position: Vec3::new(end.x, end.y, end.z + params.signal_head_height_m),
                    kind: SignalHeadKind::Vehicle,
                    group: m.approach.phase_group(),
                });
            }
            signals.push(SignalPlan {
                id: plan_id,
                junction: JunctionId::new(jid),
                cycle_s: params.cycle_s,
                offset_s: 0.0,
                controlled,
                phases: vec![
                    phase(0, false, green_s),
                    phase(0, true, params.amber_s),
                    phase(1, false, green_s),
                    phase(1, true, params.amber_s),
                ],
                heads,
            });
            junctions[jid as usize].control = JunctionControl::Signalised { plan: plan_id };
        }
    }

    // --- Crossings ------------------------------------------------------------------
    let mut crossings: Vec<Crossing> = Vec::new();
    if params.crossings {
        for jid in 0..junctions.len() as u32 {
            let (col, row) = layout.cell(JunctionId::new(jid));
            let (cx, cy) = layout.junction_xy(col, row);
            for d in Dir4::ALL {
                if layout.neighbour(col, row, d).is_none() {
                    continue;
                }
                let (fx, fy) = d.forward();
                let (rx, ry) = d.right_vector();
                let h = layout.half_m;
                crossings.push(Crossing {
                    id: CrossingId::new(crossings.len() as u32),
                    junction: JunctionId::new(jid),
                    from: Vec3::new_2d(cx + fx * h + rx * h, cy + fy * h + ry * h),
                    to: Vec3::new_2d(cx + fx * h - rx * h, cy + fy * h - ry * h),
                    width_m: params.crossing_width_m,
                    priority: true,
                });
            }
        }
    }

    // --- Buildings, one per block ----------------------------------------------------
    let mut buildings: Vec<Building> = Vec::new();
    if params.block_buildings {
        let inset = layout.half_m + params.sidewalk_m;
        for row in 0..layout.rows - 1 {
            for col in 0..layout.cols - 1 {
                let (x0, y0) = layout.junction_xy(col, row);
                let (x1, y1) = layout.junction_xy(col + 1, row + 1);
                let (west, south) = (x0 + inset, y0 + inset);
                let (east, north) = (x1 - inset, y1 - inset);
                if east - west < 1.0 || north - south < 1.0 {
                    continue;
                }
                buildings.push(Building::new(
                    BuildingId::new(buildings.len() as u32),
                    [
                        Vec3::new_2d(west, south),
                        Vec3::new_2d(east, south),
                        Vec3::new_2d(east, north),
                        Vec3::new_2d(west, north),
                    ],
                    Vec::new(),
                    params.building_height_m,
                    0.0,
                    MaterialClass::Unknown,
                    // The generator invents the height, so it is a default, and saying so
                    // is what 04-models.md §1.3 requires (invariant I-W3).
                    HeightSource::Defaulted,
                )?);
            }
        }
    }

    // --- Sites -----------------------------------------------------------------------
    let mut sites: Vec<Site> = Vec::new();
    if params.rsu_at_junctions {
        for j in &junctions {
            sites.push(Site {
                id: SiteId::new(sites.len() as u32),
                node: None,
                position: j.position,
                antenna_height_m: params.rsu_antenna_height_m,
                antenna_gain_dbi: params.rsu_antenna_gain_dbi,
                kind: SiteKind::Rsu,
                name: None,
            });
        }
    }

    // --- Land use: the whole lattice is one urban zone ---------------------------------
    let (w_m, h_m) = layout.extent();
    let landuse = vec![LanduseZone::new(
        ZoneId::new(0),
        [
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(w_m, 0.0),
            Vec3::new_2d(w_m, h_m),
            Vec3::new_2d(0.0, h_m),
        ],
        LanduseClass::Urban,
        EnvClass::Urban,
    )?];

    // --- Provenance (invariant I-W3) ---------------------------------------------------
    let mut provenance = WorldProvenance::new(
        WorldSourceKind::Procedural,
        MODEL_ID,
        opts.imported_at.clone(),
        GeoOrigin::NULL_ISLAND,
    );
    provenance
        .tool_versions
        .insert(MODEL_ID.to_string(), MODEL_VERSION.to_string());
    provenance.record(
        Transformation::new("procedural-grid")
            .with("cols", params.cols)
            .with("rows", params.rows)
            .with("block_x_m", params.block_x_m)
            .with("block_y_m", params.block_y_m)
            .with("lanes_per_direction", params.lanes_per_direction)
            .with("lane_width_m", params.lane_width_m)
            .with("sidewalk_m", params.sidewalk_m)
            .with("speed_limit_mps", params.speed_limit_mps)
            .with("signalised", params.signalised)
            .with("crossings", params.crossings)
            .with("block_buildings", params.block_buildings),
    );
    provenance.record(
        Transformation::new("local-tangent-plane")
            .with("projection", crate::model::Projection::NAME)
            .with("origin_lat_deg", GeoOrigin::NULL_ISLAND.lat_deg)
            .with("origin_lon_deg", GeoOrigin::NULL_ISLAND.lon_deg)
            .with("note", "a procedural world has no real location (D6)"),
    );
    provenance.record(
        Transformation::new("quantise")
            .with("position_m", crate::quant::Q_POSITION_M)
            .with("height_m", crate::quant::Q_HEIGHT_M)
            .with("speed_mps", crate::quant::Q_SPEED_MPS)
            .with("time_s", crate::quant::Q_TIME_S)
            .with("db", crate::quant::Q_DB),
    );
    if params.block_buildings {
        provenance.record(
            Transformation::new("height-default")
                .with("rule", "constant")
                .with("default_height_m", params.building_height_m)
                .with("calibration", "TODO: calibrate (04-models §1.3)"),
        );
    }
    for layer in ["roads", "buildings", "landuse"] {
        provenance
            .layers
            .push(LayerLicence::new(layer, "Apache-2.0"));
    }
    provenance.notes.push(
        "Generated geometry: no third-party data, so no attribution is required.".to_string(),
    );
    // The payload drops what the renderer does not need; recording it is invariant I-W3.
    provenance.record_dropped("building_holes", 0);

    let roads = RoadNetwork::new(lanes, edges, junctions, connections, crossings)?;
    World::builder(GeoOrigin::NULL_ISLAND)
        .roads(roads)
        .buildings(buildings)
        .signals(signals)
        .sites(sites)
        .landuse(landuse)
        .default_env(opts.default_env)
        .symbols(symbols)
        .provenance(provenance)
        .index_options(opts.index_options)
        .build()
}

/// Builds a junction's conflict matrix from its movements.
///
/// Two movements are **foes** when they cannot both be taken at once:
///
/// * they end on the same lane — a merge, even if their paths only meet at the end; or
/// * their connector polylines cross.
///
/// Two movements that *start* on the same lane are never foes: they are a divergence, one
/// vehicle takes one of them, and marking them as conflicting would block a junction
/// against itself.
///
/// The **response** (who gives way) follows two rules, applied in order:
///
/// 1. A movement that crosses opposing traffic — a left turn or a U-turn in right-hand
///    traffic — gives way to one that does not.
/// 2. Otherwise, priority to the right: of two conflicting movements of equal rank, the
///    one whose approach comes from the other's right has priority.
///
/// A pair the rules leave unresolved — two *opposing* left turns, which cross in this
/// geometry and rank equally — is marked as a conflict with **no** yield in either
/// direction, and the intersection-control model must resolve it. Recording the conflict
/// while admitting the rules do not settle it is better than inventing a precedence that
/// no highway code supports.
fn conflict_matrix(movements: &[Movement], lanes: &[Lane]) -> ConflictMatrix {
    let mut matrix = ConflictMatrix::new(movements.len());
    let rank = |m: &Movement| u8::from(!m.turn.crosses_opposing_traffic());
    for a in 0..movements.len() {
        for b in a + 1..movements.len() {
            let (ma, mb) = (&movements[a], &movements[b]);
            if ma.from_lane == mb.from_lane {
                continue;
            }
            let merges = ma.to_lane == mb.to_lane;
            let crosses = crate::index::polylines_cross(
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
            } else if mb.approach == ma.approach.left() {
                // `b` approaches from `a`'s right.
                matrix.set_response(a, b, true);
            } else if ma.approach == mb.approach.left() {
                matrix.set_response(b, a, true);
            }
        }
    }
    matrix
}

/// The [`WorldSource`] plug-in wrapper around [`grid`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GridSource;

impl GridSource {
    /// A new source. It holds no state: the generator is a pure function.
    pub fn new() -> Self {
        Self
    }
}

impl WorldSource for GridSource {
    fn card(&self) -> ModelCard {
        card()
    }

    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World> {
        match src {
            WorldSourceSpec::Procedural { generator, params }
                if generator == MODEL_ID || generator == "procedural-grid" =>
            {
                let params: GridParams = serde_json::from_value(params.clone())?;
                grid(&params, opts)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

/// The model card of `world/source/procedural-grid` (03-interfaces.md §12).
///
/// Every parameter [`grid`] reads appears here with its unit, its default and where that
/// default comes from (invariant I-C3). The values that no source supports are marked
/// `todo-calibrate` with a plan, which is what registry rule R1 requires and what puts
/// them on the generated "todo-calibrate" page rather than letting them pass as facts.
pub fn card() -> ModelCard {
    let legacy = || {
        Source::new(
            SourceKind::Code,
            "legacy engine run.py L412-424, via 04-models.md §1.2 preset `legacy`",
        )
    };
    let tr36885 = || {
        Source::new(
            SourceKind::Standard,
            "3GPP TR 36.885 Table A.1.2-1, via 04-models.md §1.2 preset `tr36885-urban`",
        )
    };
    let todo = |what: &str, plan: &str| {
        let mut p = Parameter::new(
            what.to_string(),
            String::new(),
            serde_json::Value::Null,
            Source::todo_calibrate(format!("procedural-grid {what}")),
        );
        p.calibration = Some(plan.to_string());
        p
    };
    let mut parameters = vec![
        Parameter::new("cols", "-", 6.into(), legacy()),
        Parameter::new("rows", "-", 6.into(), legacy()),
        Parameter::new("block_x_m", "m", 120.0.into(), legacy()),
        Parameter::new("block_y_m", "m", 120.0.into(), legacy()),
        Parameter::new("lanes_per_direction", "-", 1.into(), legacy()),
        Parameter::new("lane_width_m", "m", 3.5.into(), legacy()),
        Parameter::new("sidewalk_m", "m", 0.0.into(), tr36885()),
        Parameter::new("signalised", "-", false.into(), legacy()),
        Parameter::new("crossings", "-", false.into(), legacy()),
        Parameter::new("block_buildings", "-", false.into(), legacy()),
        Parameter::new("rsu_at_junctions", "-", false.into(), legacy()),
    ];
    let mut push = |mut p: Parameter, unit: &str, default: serde_json::Value| {
        p.unit = unit.to_string();
        p.default = default;
        parameters.push(p);
    };
    push(
        todo(
            "speed_limit_mps",
            "take the urban default speed limit from the `maxspeed` distribution of the \
             Phase 2 city bounding boxes and record the median; 13.89 m/s (50 km/h) is a \
             placeholder, not a measurement",
        ),
        "m/s",
        13.89.into(),
    );
    push(
        todo(
            "cycle_s",
            "fit the cycle length and split to the fundamental-diagram targets of \
             04-models.md §2.9 once the mobility tier can measure junction throughput",
        ),
        "s",
        60.0.into(),
    );
    push(
        todo(
            "amber_s",
            "same study as `cycle_s`; the highway codes that specify an amber time give \
             3-5 s depending on the approach speed, which is a speed-dependent rule this \
             fixed-time generator does not yet implement",
        ),
        "s",
        3.0.into(),
    );
    push(
        todo(
            "signal_head_height_m",
            "measure mast-arm mounting heights from three street-level imagery samples \
             per Phase 2 city and record the median; the value affects rendering and the \
             RSU-to-signal line of sight, nothing else",
        ),
        "m",
        5.0.into(),
    );
    push(
        todo(
            "crossing_width_m",
            "measure painted crossing widths from the Phase 2 city extracts, where OSM \
             `crossing` ways carry a width tag",
        ),
        "m",
        4.0.into(),
    );
    push(
        todo(
            "building_height_m",
            "the height-defaulting study of 04-models.md §1.3: regress \
             Microsoft-estimated heights on OSM `building:levels` for the Phase 2 boxes \
             and record the per-land-use default",
        ),
        "m",
        20.0.into(),
    );
    push(
        todo(
            "rsu_antenna_height_m",
            "no source consulted so far states an RSU mast height; take the distribution \
             from a deployment report or from the ETSI/3GPP evaluation assumptions and \
             cite it",
        ),
        "m",
        6.0.into(),
    );
    push(
        todo(
            "rsu_antenna_gain_dbi",
            "take it from the antenna datasheet of whichever RSU the Phase 3 hardware \
             profile models",
        ),
        "dBi",
        5.0.into(),
    );

    ModelCard {
        tier: vec![Tier::Abstract, Tier::Medium, Tier::High],
        equations: vec![
            Equation::new(
                "lane offset",
                "d_k = (n − 0.5 − k) · w, to the right of travel",
            ),
            Equation::new(
                "turn geometry",
                "B(t) = (1−t)² P₀ + 2(1−t)t C + t² P₁, C at the intersection of the two \
                 tangent lines, sampled at 7 points",
            ),
            Equation::new(
                "junction position",
                "(x_i, y_j) = (half + i · block_x, half + j · block_y), half = n · w",
            ),
        ],
        parameters,
        assumptions: vec![
            "Right-hand traffic: lane 0 is the rightmost in the direction of travel, an \
             east-bound lane lies south of its street's centreline."
                .to_string(),
            "Every street is two-way with the same number of lanes each way, every \
             junction is a right-angled crossroads, and the ground is flat at z = 0."
                .to_string(),
            "Turning movements are right from the rightmost lane, straight from every \
             lane, left from the leftmost. No U-turns are generated."
                .to_string(),
            "Signalisation, when on, is a two-phase fixed-time plan with permissive left \
             turns and no coordination between junctions (offset 0 everywhere)."
                .to_string(),
            "A procedural world has no real location: its origin is (0, 0) and its \
             projection is nominal."
                .to_string(),
        ],
        limitations: vec![
            "A perfect lattice is not a city: no dead ends, no one-way streets, no \
             varying block sizes, no dual carriageways, no gradient. Use \
             `world/source/osm` for a real network."
                .to_string(),
            "Two opposing left turns are marked as conflicting with no precedence, \
             because neither the turn ranking nor priority-to-the-right settles them."
                .to_string(),
            "Buildings, when generated, are one box per block at a single invented \
             height, which is a propagation obstacle of the right shape and the wrong \
             size."
                .to_string(),
        ],
        ignores: vec![
            "Sidewalks are a setback for the buildings, not lanes: no pedestrian network \
             is generated."
                .to_string(),
            "Terrain: the generator produces no DEM, and every z is 0.".to_string(),
        ],
        sources: vec![legacy(), tr36885()],
        validation: Validation::new(ValidationStatus::Unvalidated),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            MODEL_ID,
            Family::World,
            MODEL_VERSION,
            "A rectangular lattice of two-way streets with signalised or priority \
             junctions, generated deterministically from its parameters: the reference \
             world for tests, calibration runs and the TR 36.885 urban grid scenario.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_algebra() {
        for d in Dir4::ALL {
            assert_eq!(d.left().right(), d);
            assert_eq!(d.opposite().opposite(), d);
            assert_eq!(d.left().left().left().left(), d);
            let (fx, fy) = d.forward();
            let (rx, ry) = d.right_vector();
            assert_eq!(fx * rx + fy * ry, 0.0, "right is perpendicular to forward");
            // Right of travel is forward rotated clockwise: east's right is south.
            assert_eq!((rx, ry), d.right().forward());
        }
        assert_eq!(Dir4::East.right(), Dir4::South);
        assert_eq!(Dir4::East.left(), Dir4::North);
        assert_eq!(Dir4::East.step(), (1, 0));
        assert_eq!(Dir4::South.step(), (0, -1));
        assert_eq!(Dir4::East.phase_group(), Dir4::West.phase_group());
        assert_ne!(Dir4::East.phase_group(), Dir4::North.phase_group());
    }

    #[test]
    fn lane_offsets_put_traffic_on_the_right() {
        let layout = Layout::new(&GridParams::legacy());
        // One lane per direction, 3.5 m wide: each lane centre is half a width from the
        // street centreline, east-bound to the south and west-bound to the north.
        assert_eq!(layout.lane_offset_m(0), 1.75);
        let east = layout.departure_point(0, 0, Dir4::East, 0);
        let west = layout.departure_point(0, 0, Dir4::West, 0);
        let (cx, cy) = layout.junction_xy(0, 0);
        assert_eq!(east.y, cy - 1.75);
        assert_eq!(west.y, cy + 1.75);
        assert_eq!(east.x, cx + layout.half_m);
        assert_eq!(west.x, cx - layout.half_m);

        let mut two = GridParams::legacy();
        two.lanes_per_direction = 2;
        let layout = Layout::new(&two);
        assert_eq!(layout.lane_offset_m(0), 5.25, "lane 0 is the kerb lane");
        assert_eq!(
            layout.lane_offset_m(1),
            1.75,
            "lane 1 is next to the centreline"
        );
    }

    #[test]
    fn lattice_neighbours_stop_at_the_edge() {
        let layout = Layout::new(&GridParams::legacy().with_size(3, 3));
        assert_eq!(layout.neighbour(1, 1, Dir4::East), Some((2, 1)));
        assert_eq!(layout.neighbour(2, 1, Dir4::East), None);
        assert_eq!(layout.neighbour(0, 0, Dir4::South), None);
        assert_eq!(layout.junction_id(2, 1), JunctionId::new(5));
        assert_eq!(layout.cell(JunctionId::new(5)), (2, 1));
        let (w, h) = layout.extent();
        assert_eq!(w, 2.0 * 3.5 + 240.0);
        assert_eq!(h, w);
    }

    #[test]
    fn bezier_passes_through_its_ends_and_bends_the_right_way() {
        let start = Vec3::new_2d(0.0, 0.0);
        let end = Vec3::new_2d(10.0, 10.0);
        let control = turn_control(start, end, Dir4::East);
        assert_eq!(
            control,
            Vec3::new_2d(10.0, 0.0),
            "the tangents meet east of the start"
        );
        let curve = bezier(start, control, end, 7);
        assert_eq!(curve.len(), 7);
        assert_eq!(curve[0], start);
        assert_eq!(curve[6], end);
        // Every interior point is inside the control triangle's bounding box and bows
        // towards the control point.
        for point in &curve[1..6] {
            assert!(point.x > 0.0 && point.x < 10.0);
            assert!(point.y > 0.0 && point.y < 10.0);
            assert!(point.x > point.y, "an east-to-north turn bows east first");
        }
        assert_eq!(
            bezier(start, control, end, 1).len(),
            2,
            "two points minimum"
        );
    }

    #[test]
    fn presets_are_the_documented_ones() {
        let legacy = GridParams::legacy();
        assert_eq!((legacy.cols, legacy.rows), (6, 6));
        assert_eq!(legacy.block_x_m, 120.0);
        assert_eq!(legacy.lanes_per_direction, 1);
        assert_eq!(legacy.lane_width_m, 3.5);
        assert_eq!(GridParams::default(), legacy);

        let tr = GridParams::tr36885_urban();
        assert_eq!(tr.block_x_m, 433.0);
        assert_eq!(tr.block_y_m, 250.0);
        assert_eq!(tr.lanes_per_direction, 2);
        assert_eq!(tr.sidewalk_m, 3.0);
        // TR 36.885's minimum urban area is 1 299 m × 750 m of street.
        let layout = Layout::new(&tr);
        let (w, h) = layout.extent();
        assert!(w >= 1_299.0 && h >= 750.0, "{w} × {h} m");
        // And its 20 m street width is the carriageway plus two sidewalks.
        assert_eq!(2.0 * tr.half_width_m() + 2.0 * tr.sidewalk_m, 20.0);
    }

    #[test]
    fn parameters_deserialise_with_defaults_and_reject_typos() {
        let from_empty: GridParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(from_empty, GridParams::legacy());
        let partial: GridParams = serde_json::from_value(serde_json::json!({"cols": 9})).unwrap();
        assert_eq!(partial.cols, 9);
        assert_eq!(partial.rows, GridParams::legacy().rows);
        assert!(serde_json::from_value::<GridParams>(serde_json::json!({"colz": 9})).is_err());
    }

    #[test]
    fn the_grid_is_a_pure_function_of_its_parameters() {
        let params = GridParams::legacy().with_size(3, 3);
        let a = grid(&params, &ImportOptions::default().imported_at("2026-09-18")).unwrap();
        let b = grid(&params, &ImportOptions::default().imported_at("2026-09-18")).unwrap();
        assert_eq!(a, b, "same parameters, same options, same world");
        assert_eq!(
            crate::serde_vwp::write(&a).unwrap().bytes,
            crate::serde_vwp::write(&b).unwrap().bytes
        );

        // A different import date leaves the geometry — and so the content hash — alone,
        // but it does change the payload, because the payload embeds the provenance
        // document and the date is part of it.
        let dated = grid(&params, &ImportOptions::default().imported_at("1999-01-01")).unwrap();
        assert_eq!(dated.content_hash, a.content_hash);
        assert_ne!(
            crate::serde_vwp::write(&dated).unwrap().bytes,
            crate::serde_vwp::write(&a).unwrap().bytes
        );
    }
}
