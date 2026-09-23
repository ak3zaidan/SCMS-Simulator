//! `world/source/procedural-radial` — the spider generator (04-models.md §1.2).
//!
//! The classic radial city: `arms` spokes from a central plaza, crossed by `rings`
//! concentric ring roads `ring_spacing_m` apart. It is the geometry the legacy engine's
//! `spider_graph(arms, rings, block)` produced (`roads.py` L683-702), promoted from a
//! node-and-edge pair to the lane-level model through
//! [`crate::procedural::graph::build_lane_graph`].
//!
//! ```text
//!            ____
//!          /  |   \          arms = 6, rings = 2
//!         /---+---\
//!        | \  |  / |         the centre is node 0
//!         \---+---/          ring node (r, a) is 1 + (r−1)·arms + a
//!          \__|__/
//! ```
//!
//! # Geometry, and what it is not
//!
//! Ring roads are **chords**, not arcs: ring `r` is a regular polygon of `arms` sides
//! through the ring nodes, exactly as the legacy generator drew it. A real ring road
//! curves, and a chord ring with few arms is visibly a polygon; the honest reading is that
//! this is a radial *topology* generator whose geometry is polygonal, and the model card
//! says so.
//!
//! # Determinism
//!
//! No random number is drawn. The `dropout` parameter of 04-models.md §1.2 — the fraction
//! of ring edges removed, to make the network less regular — selects its edges by
//! deterministic decimation rather than by the legacy engine's random draw, so the world
//! stays a pure function of its parameters with no seed to record. The parameter's
//! *meaning* is the legacy one; the selection rule is this generator's and is documented
//! on the card.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;
use v2xw_core::math;

use crate::error::{Result, WorldError};
use crate::model::{GeoOrigin, RoadClass, Transformation, World, WorldProvenance, WorldSourceKind};
use crate::procedural::graph::{GraphEdge, GraphOptions, build_lane_graph, into_world};
use crate::{ImportOptions, WorldSource, WorldSourceSpec};

/// The model id of this generator (04-models.md §1.2).
pub const MODEL_ID: &str = "world/source/procedural-radial";

/// The generator's own version, as the model card reports it.
pub const MODEL_VERSION: &str = "1.0.0";

/// The legacy source both the geometry and the parameter names come from.
const LEGACY_SPIDER: &str = "legacy engine roads.py L683-702 spider_graph(arms, rings, block), \
                             with arms/rings/block from grid_w/grid_h/grid_block_m \
                             (04-models.md §1.2)";

/// The `netgenerate` recipe the legacy SUMO tooling used for its spider maps.
const NETGENERATE_SPIDER: &str = "legacy/reference/sumo/mapgen.py L234-241: netgenerate \
                                  --spider --spider.arm-number --spider.circle-number \
                                  --spider.space-radius 100 --no-turnarounds \
                                  --default.lanenumber 2, keys such as spider_8a4c";

/// The parameters of `world/source/procedural-radial`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RadialParams {
    /// Radial spokes from the centre. At least 3, as the legacy generator's
    /// `max(3, arms)` required.
    pub arms: u32,
    /// Concentric rings. At least 1.
    pub rings: u32,
    /// Distance between successive rings, metres — the legacy `block`, and
    /// `netgenerate --spider.space-radius`.
    pub ring_spacing_m: f64,
    /// Lanes per direction on every street.
    pub lanes_per_direction: u32,
    /// Lane width, metres.
    pub lane_width_m: f64,
    /// Speed limit on every street, m/s.
    pub speed_limit_mps: f64,
    /// The fraction of **ring** edges to remove, in `[0, 1)`: the legacy `grid_dropout`,
    /// which 04-models.md §1.2 records as this generator's irregularity control.
    ///
    /// Radial spokes are never removed: dropping one would disconnect everything outside
    /// it from the centre along that arm, and the point of the parameter is to break the
    /// ring regularity.
    pub dropout: f64,
    /// Whether every junction with at least three arms gets a fixed-time signal plan.
    pub signalised: bool,
    /// Signal cycle length, seconds.
    pub cycle_s: f64,
    /// Amber time at the end of each green, seconds.
    pub amber_s: f64,
    /// Height of a signal lantern above the road, metres.
    pub signal_head_height_m: f64,
    /// The junction radius as a multiple of the widest arm's half-carriageway.
    pub junction_radius_factor: f64,
}

impl Default for RadialParams {
    /// The `legacy` preset.
    fn default() -> Self {
        RadialParams::legacy()
    }
}

impl RadialParams {
    /// The `legacy` preset: the legacy engine's own defaults for a spider network
    /// (`grid_w` 6 arms, `grid_h` 6 rings, `grid_block_m` 120, `n_lanes` 1,
    /// `lane_width_m` 3.5, `grid_dropout` 0; 04-models.md §1.2).
    pub fn legacy() -> Self {
        Self {
            arms: 6,
            rings: 6,
            ring_spacing_m: 120.0,
            lanes_per_direction: 1,
            lane_width_m: 3.5,
            speed_limit_mps: 13.89,
            dropout: 0.0,
            signalised: false,
            cycle_s: 60.0,
            amber_s: 3.0,
            signal_head_height_m: 5.0,
            junction_radius_factor: 1.5,
        }
    }

    /// The `netgenerate-spider` preset: the `spider_8a4c` key of the legacy SUMO
    /// tooling — 8 arms, 4 circles, `--spider.space-radius 100`,
    /// `--default.lanenumber 2` (`mapgen.py` L6, L234-241).
    pub fn netgenerate_spider() -> Self {
        Self {
            arms: 8,
            rings: 4,
            ring_spacing_m: 100.0,
            lanes_per_direction: 2,
            ..RadialParams::legacy()
        }
    }

    /// The same parameters with a different size.
    #[must_use]
    pub fn with_size(mut self, arms: u32, rings: u32) -> Self {
        self.arms = arms;
        self.rings = rings;
        self
    }

    /// The same parameters with signals on or off.
    #[must_use]
    pub fn with_signals(mut self, signalised: bool) -> Self {
        self.signalised = signalised;
        self
    }

    /// Checks that the parameters describe a world that can exist.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`], naming the parameter.
    pub fn validate(&self) -> Result<()> {
        let bad = |parameter: &str, problem: String| WorldError::InvalidParameter {
            parameter: parameter.to_string(),
            problem,
        };
        if self.arms < 3 {
            return Err(bad(
                "arms",
                format!("{} arms; a radial network needs at least 3", self.arms),
            ));
        }
        if self.rings < 1 {
            return Err(bad(
                "rings",
                "a radial network needs at least 1 ring".to_string(),
            ));
        }
        if !(self.ring_spacing_m.is_finite() && self.ring_spacing_m > 0.0) {
            return Err(bad(
                "ring_spacing_m",
                format!("{} m is not a positive spacing", self.ring_spacing_m),
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
        if !(self.speed_limit_mps.is_finite() && self.speed_limit_mps > 0.0) {
            return Err(bad(
                "speed_limit_mps",
                format!("{} m/s is not a positive speed", self.speed_limit_mps),
            ));
        }
        if !(self.dropout.is_finite() && (0.0..1.0).contains(&self.dropout)) {
            return Err(bad(
                "dropout",
                format!("{} is not a fraction in [0, 1)", self.dropout),
            ));
        }
        Ok(())
    }

    /// The index of ring node `(r, a)`, `r` counted from 1 at the innermost ring.
    ///
    /// Node `0` is the centre, so ring node `(r, a)` is `1 + (r − 1)·arms + a`, which is
    /// the legacy generator's own numbering.
    pub fn node_index(&self, ring: u32, arm: u32) -> usize {
        1 + ((ring - 1) * self.arms + arm % self.arms) as usize
    }

    /// How many nodes the network has: the centre plus `rings × arms`.
    pub fn node_count(&self) -> usize {
        1 + (self.rings * self.arms) as usize
    }

    /// The graph options this generator hands the shared builder.
    fn graph_options(&self) -> GraphOptions {
        GraphOptions {
            signalised: self.signalised,
            cycle_s: self.cycle_s,
            amber_s: self.amber_s,
            signal_head_height_m: self.signal_head_height_m,
            junction_radius_factor: self.junction_radius_factor,
            ..GraphOptions::default()
        }
    }
}

/// The node positions and undirected edges of a radial network, before any lane geometry.
///
/// Exposed because it is the legacy `spider_graph` return value, and because a test can
/// check the topology without going through the lane builder. The centre is at
/// `(span, span)` with `span = rings · ring_spacing_m`, so every coordinate is
/// non-negative before the lane builder's own margin is added.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] for parameters [`RadialParams::validate`] rejects.
pub fn radial_graph(params: &RadialParams) -> Result<(Vec<Vec3>, Vec<GraphEdge>)> {
    params.validate()?;
    let span = f64::from(params.rings) * params.ring_spacing_m;
    let mut nodes: Vec<Vec3> = Vec::with_capacity(params.node_count());
    nodes.push(Vec3::new_2d(span, span));
    for ring in 1..=params.rings {
        for arm in 0..params.arms {
            let theta = core::f64::consts::TAU * f64::from(arm) / f64::from(params.arms);
            let (sin, cos) = math::sin_cos(theta);
            let radius = f64::from(ring) * params.ring_spacing_m;
            nodes.push(Vec3::new_2d(span + radius * cos, span + radius * sin));
        }
    }

    let street = |a: usize, b: usize, class: RoadClass, name: String| {
        GraphEdge::new(a, b)
            .with_lanes(params.lanes_per_direction)
            .with_speed_mps(params.speed_limit_mps)
            .with_class(class)
            .with_name(name)
    };
    let mut edges: Vec<GraphEdge> = Vec::new();
    // Radial spokes first, arm by arm, centre outwards: the legacy edge order.
    for arm in 0..params.arms {
        let name = format!("Spoke {arm}");
        edges.push(street(
            0,
            params.node_index(1, arm),
            RoadClass::Primary,
            name.clone(),
        ));
        for ring in 1..params.rings {
            edges.push(street(
                params.node_index(ring, arm),
                params.node_index(ring + 1, arm),
                RoadClass::Primary,
                name.clone(),
            ));
        }
    }
    // Then the rings, innermost first.
    let mut ring_index = 0usize;
    for ring in 1..=params.rings {
        let name = format!("Ring {ring}");
        for arm in 0..params.arms {
            // Deterministic decimation: edge `i` of the ring sequence is removed when the
            // running count `floor((i + 1)·dropout)` steps up, which removes exactly
            // `round(dropout · n)` of `n` edges, spread evenly, with no random draw.
            let before = (ring_index as f64 * params.dropout).floor();
            let after = ((ring_index + 1) as f64 * params.dropout).floor();
            ring_index += 1;
            if after > before {
                continue;
            }
            edges.push(street(
                params.node_index(ring, arm),
                params.node_index(ring, arm + 1),
                RoadClass::Secondary,
                name.clone(),
            ));
        }
    }
    Ok((nodes, edges))
}

/// Builds a radial world (04-models.md §1.2, model id [`MODEL_ID`]).
///
/// Deterministic from `params` alone: no random draws, no wall clock, no hash iteration.
///
/// # Errors
///
/// Whatever [`RadialParams::validate`] or the shared lane builder rejects.
pub fn radial(params: &RadialParams, opts: &ImportOptions) -> Result<World> {
    let (nodes, edges) = radial_graph(params)?;
    let graph = build_lane_graph(&nodes, &edges, &params.graph_options())?;
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
        Transformation::new("procedural-radial")
            .with("arms", params.arms)
            .with("rings", params.rings)
            .with("ring_spacing_m", params.ring_spacing_m)
            .with("lanes_per_direction", params.lanes_per_direction)
            .with("lane_width_m", params.lane_width_m)
            .with("speed_limit_mps", params.speed_limit_mps)
            .with("dropout", params.dropout)
            .with("dropout_rule", "deterministic decimation, no random draw")
            .with("signalised", params.signalised)
            .with("ring_geometry", "chords, not arcs"),
    );
    into_world(graph, provenance, opts)
}

/// The [`WorldSource`] plug-in wrapper around [`radial`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RadialSource;

impl RadialSource {
    /// A new source. It holds no state: the generator is a pure function.
    pub fn new() -> Self {
        Self
    }
}

impl WorldSource for RadialSource {
    fn card(&self) -> ModelCard {
        card()
    }

    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World> {
        match src {
            WorldSourceSpec::Procedural { generator, params }
                if generator == MODEL_ID
                    || generator == "procedural-radial"
                    || generator == "procedural-spider" =>
            {
                let params: RadialParams = if params.is_null() {
                    RadialParams::default()
                } else {
                    serde_json::from_value(params.clone())?
                };
                radial(&params, opts)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

/// The model card of `world/source/procedural-radial` (03-interfaces.md §12).
pub fn card() -> ModelCard {
    let legacy = || Source::new(SourceKind::Code, LEGACY_SPIDER);
    let netgenerate = || Source::new(SourceKind::Code, NETGENERATE_SPIDER);
    let todo = |name: &str, unit: &str, default: serde_json::Value, plan: &str| Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: None,
        source: Source::todo_calibrate(format!("procedural-radial {name}")),
        calibration: Some(plan.to_string()),
    };
    ModelCard {
        tier: vec![Tier::Abstract, Tier::Medium, Tier::High],
        equations: vec![
            Equation::new(
                "ring node position",
                "(x, y) = (span + r·s·cos(2πa/arms), span + r·s·sin(2πa/arms)), \
                 span = rings·s",
            ),
            Equation::new(
                "ring chord length",
                "L = 2·r·s·sin(π/arms) — the polygon side, not the arc",
            ),
            Equation::new(
                "dropout selection",
                "ring edge i is removed when floor((i+1)·dropout) > floor(i·dropout)",
            ),
        ],
        parameters: vec![
            Parameter::new("arms", "-", 6.into(), legacy()),
            Parameter::new("rings", "-", 6.into(), legacy()),
            Parameter::new("ring_spacing_m", "m", 120.0.into(), legacy()),
            Parameter::new("lanes_per_direction", "-", 1.into(), legacy()),
            Parameter::new("lane_width_m", "m", 3.5.into(), legacy()),
            Parameter::new("dropout", "-", 0.0.into(), legacy()),
            Parameter::new("signalised", "-", false.into(), legacy()),
            Parameter::new("netgenerate_spider.arms", "-", 8.into(), netgenerate()),
            Parameter::new(
                "netgenerate_spider.ring_spacing_m",
                "m",
                100.0.into(),
                netgenerate(),
            ),
            todo(
                "speed_limit_mps",
                "m/s",
                13.89.into(),
                "13.89 m/s is 50 km/h, a placeholder and not a measurement; take the urban \
                 default from the `maxspeed` distribution of the Phase 2 city bounding \
                 boxes and record the median, as the grid generator's card also plans",
            ),
            todo(
                "cycle_s",
                "s",
                60.0.into(),
                "fit the cycle length and split to the fundamental-diagram targets of \
                 04-models.md §2.9 once the mobility tier can measure junction throughput",
            ),
            todo(
                "amber_s",
                "s",
                3.0.into(),
                "same study as cycle_s; the highway codes that specify an amber time give \
                 3-5 s depending on approach speed, which is a speed-dependent rule this \
                 fixed-time generator does not implement",
            ),
            todo(
                "signal_head_height_m",
                "m",
                5.0.into(),
                "measure mast-arm mounting heights from three street-level imagery samples \
                 per Phase 2 city and record the median",
            ),
            todo(
                "junction_radius_factor",
                "-",
                1.5.into(),
                "the junction area as a multiple of the widest arm's half-carriageway. No \
                 source in the cache states one; measure the junction radii netconvert \
                 computes for the Phase 2 extracts against their carriageway widths and \
                 fit the factor",
            ),
        ],
        assumptions: vec![
            "Right-hand traffic: lane 0 is the rightmost in the direction of travel.".to_string(),
            "Every street is two-way with the same number of lanes each way, and the \
             ground is flat at z = 0."
                .to_string(),
            "Ring roads are chords through the ring nodes, so ring `r` is a regular \
             `arms`-gon rather than a circle."
                .to_string(),
            "Turning movements are right from the kerb lane, straight on from every lane, \
             left from the median lane; no turnarounds are generated, which is \
             netgenerate's --no-turnarounds."
                .to_string(),
            "The dropout selection is deterministic decimation. The legacy engine drew the \
             removed edges at random; this generator does not, so that a radial world needs \
             no seed and is a pure function of its parameters."
                .to_string(),
        ],
        limitations: vec![
            "The centre is a single junction with `arms` approaches. At eight arms and two \
             lanes each way its connectors are geometrically crowded: a real radial city \
             has a roundabout or a plaza there, and this generator has neither."
                .to_string(),
            "A chord ring with few arms is visibly a polygon, and its junction angles are \
             the polygon's, not a circle's."
                .to_string(),
            "No buildings, no crossings, no sidewalks and no RSU sites: the grid generator \
             offers those, this one does not."
                .to_string(),
        ],
        ignores: vec![
            "Terrain: every z is 0 until a DEM is attached (see `crate::dem`).".to_string(),
        ],
        sources: vec![legacy(), netgenerate()],
        validation: Validation::new(ValidationStatus::Unvalidated),
        determinism: Determinism {
            uses_rng: false,
            rng_domains: Vec::new(),
        },
        ..ModelCard::new(
            MODEL_ID,
            Family::World,
            MODEL_VERSION,
            "The classic radial city: spokes from a central plaza crossed by concentric \
             ring roads, generated deterministically from its parameters.",
        )
    }
}
