//! `world/source/procedural-random` — the random-growth generator (04-models.md §1.2).
//!
//! The third procedural topology the legacy SUMO tooling used, beside the grid and the
//! spider: `netgenerate --rand`, grown from one node by repeatedly attaching a new
//! junction at a random heading and a random distance
//! (`legacy/reference/sumo/mapgen.py` L242-249, keys such as `rand_150`).
//!
//! It exists because a grid and a spider are both regular, and a regular network hides
//! whole classes of defect: every junction has the same arm count, every block the same
//! length, every approach the same angle. A random network has three-arm junctions,
//! oblique approaches, cul-de-sacs and short blocks, which is what a routing, a junction
//! and a propagation model each need to be exercised on before a real city is trusted.
//!
//! # The growth rule
//!
//! Every iteration makes up to `tries_per_iteration` attempts, and each attempt draws, in
//! this order and from this one stream:
//!
//! 1. an existing node, uniformly (`stream.below(n)`) — re-drawn on every attempt, so a
//!    rejected position does not tie the iteration to one junction;
//! 2. a heading, uniformly in `[0, 2π)`;
//! 3. a distance, uniformly in `[min_distance_m, max_distance_m]`.
//!
//! The attempt is then resolved:
//!
//! * if the candidate position lands within `min_distance_m` of an existing node, the
//!   attempt becomes a **loop-closing** street to the nearest such node instead of a new
//!   junction — this is what stops the network from being a tree;
//! * it is rejected if the new street would cross an existing one, if it would leave an
//!   angle below `min_angle_deg` at either end, if the two junctions are already joined,
//!   or if it would leave the optional radius bound;
//! * the first attempt that survives ends the iteration.
//!
//! # Determinism
//!
//! Every draw comes from one [`RngStream`](v2xw_core::RngStream) checked out of an
//! [`RngRegistry`] keyed by `(RngDomain::plugin(MODEL_ID), EntityRef::custom(MODEL_ID, 0))`
//! — the plug-in domain of 02-architecture.md §6.2, because a world generator is a
//! `WorldSource` plug-in and `v2xw-core` has no world-generation domain of its own. The
//! stream is seeded from [`RandomParams::seed`] alone, the draws happen in the documented
//! order above, and the resulting node list is then handed to the same pure lane builder
//! the radial generator uses. Two runs with the same seed therefore produce the same
//! world, byte for byte, on every platform (conformance item W5).

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::Vec3;
use v2xw_core::math;
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};

use crate::error::{Result, WorldError};
use crate::model::{
    GeoOrigin, RoadClass, Transformation, World, WorldProvenance, WorldSourceKind, normalise_angle,
};
use crate::procedural::graph::{GraphEdge, GraphOptions, build_lane_graph, into_world};
use crate::{ImportOptions, WorldSource, WorldSourceSpec};

/// The model id of this generator (04-models.md §1.2).
pub const MODEL_ID: &str = "world/source/procedural-random";

/// The generator's own version, as the model card reports it.
pub const MODEL_VERSION: &str = "1.0.0";

/// The recipe the parameters that have a source come from.
const NETGENERATE_RAND: &str = "legacy/reference/sumo/mapgen.py L242-249: netgenerate --rand \
                                --rand.iterations N --rand.min-distance 80 \
                                --rand.max-distance 250 --no-turnarounds \
                                --default.lanenumber 2, keys such as rand_150";

/// The calibration plan every parameter `netgenerate`'s own defaults would supply shares.
const NETGENERATE_PLAN: &str = "Only --rand.iterations, --rand.min-distance 80 and \
     --rand.max-distance 250 are attested in the cache (mapgen.py L242-249). The remaining \
     growth controls are this generator's own: read the defaults of netgenerate's --rand.* \
     options for the pinned SUMO version from `netgenerate --help`, adopt whichever \
     correspond, and record the version they came from.";

/// The parameters of `world/source/procedural-random`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RandomParams {
    /// The master seed of the stream every draw comes from.
    pub seed: u64,
    /// Growth iterations: `netgenerate --rand.iterations`. Each one adds at most one
    /// junction or one loop-closing street.
    pub iterations: u32,
    /// The shortest street the generator will create, metres, and the radius within which
    /// a candidate position is taken to *be* an existing junction rather than a new one:
    /// `netgenerate --rand.min-distance`.
    pub min_distance_m: f64,
    /// The longest street the generator will create, metres:
    /// `netgenerate --rand.max-distance`.
    pub max_distance_m: f64,
    /// The smallest angle, degrees, that two streets meeting at a junction may make.
    pub min_angle_deg: f64,
    /// How many candidate draws one iteration may make before it gives up.
    pub tries_per_iteration: u32,
    /// An optional bound on how far a junction may be from the first one, metres.
    ///
    /// `None`, the default, lets the network grow wherever the draws take it, which is
    /// what `netgenerate --rand` does. A scenario that needs the world to fit a known area
    /// sets it; it is this generator's own parameter and no source states a value, which
    /// is why the default is "no bound" rather than a number.
    pub max_radius_m: Option<f64>,
    /// Lanes per direction on every street.
    pub lanes_per_direction: u32,
    /// Lane width, metres.
    pub lane_width_m: f64,
    /// Speed limit on every street, m/s.
    pub speed_limit_mps: f64,
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

impl Default for RandomParams {
    /// The `netgenerate-rand` preset.
    fn default() -> Self {
        RandomParams::netgenerate_rand()
    }
}

impl RandomParams {
    /// The `netgenerate-rand` preset: the legacy SUMO tooling's `rand_150` key —
    /// 150 iterations, `--rand.min-distance 80`, `--rand.max-distance 250`,
    /// `--default.lanenumber 2` (`mapgen.py` L242-249).
    pub fn netgenerate_rand() -> Self {
        Self {
            seed: 1,
            iterations: 150,
            min_distance_m: 80.0,
            max_distance_m: 250.0,
            min_angle_deg: 45.0,
            tries_per_iteration: 50,
            max_radius_m: None,
            lanes_per_direction: 2,
            lane_width_m: 3.5,
            speed_limit_mps: 13.89,
            signalised: false,
            cycle_s: 60.0,
            amber_s: 3.0,
            signal_head_height_m: 5.0,
            junction_radius_factor: 1.5,
        }
    }

    /// A small network, for a test that wants a handful of oblique junctions rather than a
    /// city.
    pub fn small(seed: u64) -> Self {
        Self {
            seed,
            iterations: 12,
            ..RandomParams::netgenerate_rand()
        }
    }

    /// The same parameters with another seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// The same parameters with another iteration count.
    #[must_use]
    pub fn with_iterations(mut self, iterations: u32) -> Self {
        self.iterations = iterations;
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
        if self.iterations == 0 {
            return Err(bad(
                "iterations",
                "a network needs at least one growth iteration".to_string(),
            ));
        }
        for (name, v) in [
            ("min_distance_m", self.min_distance_m),
            ("max_distance_m", self.max_distance_m),
            ("lane_width_m", self.lane_width_m),
            ("speed_limit_mps", self.speed_limit_mps),
        ] {
            if !(v.is_finite() && v > 0.0) {
                return Err(bad(name, format!("{v} is not positive")));
            }
        }
        if self.max_distance_m < self.min_distance_m {
            return Err(bad(
                "max_distance_m",
                format!(
                    "{} m is below min_distance_m of {} m",
                    self.max_distance_m, self.min_distance_m
                ),
            ));
        }
        if !(self.min_angle_deg.is_finite() && (0.0..90.0).contains(&self.min_angle_deg)) {
            return Err(bad(
                "min_angle_deg",
                format!(
                    "{} is not an angle in [0, 90) degrees; at 90 no second street could \
                     ever join a junction",
                    self.min_angle_deg
                ),
            ));
        }
        if self.tries_per_iteration == 0 {
            return Err(bad(
                "tries_per_iteration",
                "an iteration needs at least one try".to_string(),
            ));
        }
        if self.lanes_per_direction == 0 {
            return Err(bad(
                "lanes_per_direction",
                "a street needs at least one lane per direction".to_string(),
            ));
        }
        if let Some(r) = self.max_radius_m {
            if !(r.is_finite() && r >= self.max_distance_m) {
                return Err(bad(
                    "max_radius_m",
                    format!(
                        "{r} m must be finite and at least max_distance_m of {} m, or the \
                         first street cannot be placed",
                        self.max_distance_m
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The graph options this generator hands the shared builder.
    fn graph_options(&self) -> GraphOptions {
        GraphOptions {
            signalised: self.signalised,
            cycle_s: self.cycle_s,
            amber_s: self.amber_s,
            signal_head_height_m: self.signal_head_height_m,
            junction_radius_factor: self.junction_radius_factor,
            // A street shorter than the shortest the growth rule allows cannot appear, so
            // the builder's own floor is left where it is; it only ever fires when two
            // junction areas are wide enough to meet in the middle of a minimum-length
            // street.
            ..GraphOptions::default()
        }
    }
}

/// What the growth did, iteration by iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RandomStats {
    /// Iterations run.
    pub iterations: u64,
    /// Junctions placed, the first one included.
    pub nodes: u64,
    /// Streets created.
    pub edges: u64,
    /// Streets that joined two existing junctions rather than creating one, which are the
    /// edges that make the network a graph rather than a tree.
    pub loops_closed: u64,
    /// Iterations that used up every try without placing anything.
    pub iterations_failed: u64,
    /// Candidate draws rejected because the street would have crossed another.
    pub rejected_crossing: u64,
    /// Candidate draws rejected because they would have left too sharp an angle.
    pub rejected_angle: u64,
    /// Candidate draws rejected because the two junctions were already joined.
    pub rejected_duplicate: u64,
    /// Candidate draws rejected because they would have left the radius bound.
    pub rejected_out_of_bounds: u64,
}

/// Grows the node-and-edge graph of a random network.
///
/// Returns the node positions, the undirected streets and the growth statistics. The first
/// node is at the origin and every other position is relative to it; the lane builder
/// translates the whole set so that no coordinate is negative.
///
/// # Errors
///
/// [`WorldError::InvalidParameter`] for parameters [`RandomParams::validate`] rejects, or
/// if the growth placed no street at all — which means the parameters describe a network
/// that cannot be grown, not a world with one junction in it.
pub fn random_graph(params: &RandomParams) -> Result<(Vec<Vec3>, Vec<GraphEdge>, RandomStats)> {
    params.validate()?;
    let registry = RngRegistry::new(params.seed);
    let mut stream = registry.checkout(RngDomain::plugin(MODEL_ID), EntityRef::custom(MODEL_ID, 0));

    let min_angle_rad = params.min_angle_deg * core::f64::consts::PI / 180.0;
    let mut nodes: Vec<Vec3> = vec![Vec3::new_2d(0.0, 0.0)];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut stats = RandomStats {
        iterations: u64::from(params.iterations),
        nodes: 1,
        ..RandomStats::default()
    };

    for _ in 0..params.iterations {
        let mut placed = false;
        for _ in 0..params.tries_per_iteration {
            let base = stream.below(nodes.len() as u64) as usize;
            let heading = stream.uniform(0.0, core::f64::consts::TAU);
            let distance = stream.uniform(params.min_distance_m, params.max_distance_m);
            let (sin, cos) = math::sin_cos(heading);
            let candidate = Vec3::new_2d(
                nodes[base].x + distance * cos,
                nodes[base].y + distance * sin,
            );

            // Does the candidate land on an existing junction? If it does, the attempt
            // becomes a loop-closing street to the nearest one.
            let mut target: Option<usize> = None;
            let mut nearest = f64::INFINITY;
            for (i, p) in nodes.iter().enumerate() {
                if i == base {
                    continue;
                }
                let d = p.distance_2d(candidate);
                if d < params.min_distance_m && d < nearest {
                    nearest = d;
                    target = Some(i);
                }
            }
            let closing = target.is_some();
            let (end_index, end_point) = match target {
                Some(i) => (i, nodes[i]),
                None => (nodes.len(), candidate),
            };

            if let Some(bound) = params.max_radius_m {
                if end_point.distance_2d(nodes[0]) > bound {
                    stats.rejected_out_of_bounds += 1;
                    continue;
                }
            }
            if closing {
                let key = (base.min(end_index), base.max(end_index));
                if pairs.iter().any(|p| (p.0.min(p.1), p.0.max(p.1)) == key) {
                    stats.rejected_duplicate += 1;
                    continue;
                }
                // A loop-closing street must still be long enough to be a street.
                if nodes[base].distance_2d(end_point) < params.min_distance_m {
                    stats.rejected_duplicate += 1;
                    continue;
                }
            }
            if !angle_is_clear(&nodes, &pairs, base, end_point, min_angle_rad)
                || (closing
                    && !angle_is_clear(&nodes, &pairs, end_index, nodes[base], min_angle_rad))
            {
                stats.rejected_angle += 1;
                continue;
            }
            if crosses_existing(&nodes, &pairs, base, end_index, end_point) {
                stats.rejected_crossing += 1;
                continue;
            }

            if !closing {
                nodes.push(candidate);
                stats.nodes += 1;
            } else {
                stats.loops_closed += 1;
            }
            pairs.push((base, end_index));
            stats.edges += 1;
            placed = true;
            break;
        }
        if !placed {
            stats.iterations_failed += 1;
        }
    }

    if pairs.is_empty() {
        return Err(WorldError::InvalidParameter {
            parameter: "iterations".to_string(),
            problem: format!(
                "{} iterations of {} tries placed no street at all; check \
                 min_distance_m, min_angle_deg and max_radius_m",
                params.iterations, params.tries_per_iteration
            ),
        });
    }

    let edges: Vec<GraphEdge> = pairs
        .iter()
        .enumerate()
        .map(|(i, (a, b))| {
            GraphEdge::new(*a, *b)
                .with_lanes(params.lanes_per_direction)
                .with_speed_mps(params.speed_limit_mps)
                .with_class(RoadClass::Residential)
                .with_name(format!("Street {i}"))
        })
        .collect();
    Ok((nodes, edges, stats))
}

/// True if a new street from `at` towards `towards` leaves at least `min_angle_rad`
/// between itself and every street already meeting at `at`.
fn angle_is_clear(
    nodes: &[Vec3],
    pairs: &[(usize, usize)],
    at: usize,
    towards: Vec3,
    min_angle_rad: f64,
) -> bool {
    let d = Vec3::new(towards.x - nodes[at].x, towards.y - nodes[at].y, 0.0);
    if d.norm_2d() <= 0.0 {
        return false;
    }
    let heading = math::atan2(d.y, d.x);
    for (a, b) in pairs {
        let other = if *a == at {
            *b
        } else if *b == at {
            *a
        } else {
            continue;
        };
        let e = Vec3::new(
            nodes[other].x - nodes[at].x,
            nodes[other].y - nodes[at].y,
            0.0,
        );
        if e.norm_2d() <= 0.0 {
            return false;
        }
        let existing = math::atan2(e.y, e.x);
        if normalise_angle(heading - existing).abs() < min_angle_rad {
            return false;
        }
    }
    true
}

/// True if the street `base → end` would cross a street it does not share a junction
/// with.
///
/// Streets that share a junction are excluded: they meet there by construction, and the
/// angle rule is what keeps them apart.
fn crosses_existing(
    nodes: &[Vec3],
    pairs: &[(usize, usize)],
    base: usize,
    end_index: usize,
    end_point: Vec3,
) -> bool {
    let candidate = [nodes[base], end_point];
    for (a, b) in pairs {
        if *a == base || *b == base || *a == end_index || *b == end_index {
            continue;
        }
        if crate::index::polylines_cross(&candidate, &[nodes[*a], nodes[*b]]) {
            return true;
        }
    }
    false
}

/// Builds a random world (04-models.md §1.2, model id [`MODEL_ID`]).
///
/// Deterministic from `params` — seed included — alone.
///
/// # Errors
///
/// Whatever [`RandomParams::validate`], the growth or the shared lane builder rejects.
pub fn random(params: &RandomParams, opts: &ImportOptions) -> Result<World> {
    let (nodes, edges, stats) = random_graph(params)?;
    let graph = build_lane_graph(&nodes, &edges, &params.graph_options())?;
    let mut provenance = WorldProvenance::new(
        WorldSourceKind::Procedural,
        format!("{MODEL_ID}#seed={}", params.seed),
        opts.imported_at.clone(),
        GeoOrigin::NULL_ISLAND,
    );
    provenance
        .tool_versions
        .insert(MODEL_ID.to_string(), MODEL_VERSION.to_string());
    provenance.record(
        Transformation::new("procedural-random")
            .with("seed", params.seed)
            .with("rng_domain", format!("plugin:{MODEL_ID}"))
            .with("iterations", params.iterations)
            .with("min_distance_m", params.min_distance_m)
            .with("max_distance_m", params.max_distance_m)
            .with("min_angle_deg", params.min_angle_deg)
            .with("tries_per_iteration", params.tries_per_iteration)
            .with("lanes_per_direction", params.lanes_per_direction)
            .with("lane_width_m", params.lane_width_m)
            .with("speed_limit_mps", params.speed_limit_mps)
            .with("signalised", params.signalised)
            .with("nodes_placed", stats.nodes)
            .with("edges_placed", stats.edges)
            .with("loops_closed", stats.loops_closed)
            .with("iterations_failed", stats.iterations_failed)
            .with("rejected_crossing", stats.rejected_crossing)
            .with("rejected_angle", stats.rejected_angle)
            .with("rejected_duplicate", stats.rejected_duplicate)
            .with("rejected_out_of_bounds", stats.rejected_out_of_bounds),
    );
    into_world(graph, provenance, opts)
}

/// The [`WorldSource`] plug-in wrapper around [`random`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomSource;

impl RandomSource {
    /// A new source. It holds no state: the generator is a pure function of its
    /// parameters, seed included.
    pub fn new() -> Self {
        Self
    }
}

impl WorldSource for RandomSource {
    fn card(&self) -> ModelCard {
        card()
    }

    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World> {
        match src {
            WorldSourceSpec::Procedural { generator, params }
                if generator == MODEL_ID || generator == "procedural-random" =>
            {
                let params: RandomParams = if params.is_null() {
                    RandomParams::default()
                } else {
                    serde_json::from_value(params.clone())?
                };
                random(&params, opts)
            }
            other => Err(WorldError::UnsupportedSource {
                model: MODEL_ID.to_string(),
                spec: other.label(),
            }),
        }
    }
}

/// The model card of `world/source/procedural-random` (03-interfaces.md §12).
pub fn card() -> ModelCard {
    let netgenerate = || Source::new(SourceKind::Code, NETGENERATE_RAND);
    let todo = |name: &str, unit: &str, default: serde_json::Value, plan: &str| Parameter {
        name: name.to_string(),
        unit: unit.to_string(),
        default,
        range: None,
        source: Source::todo_calibrate(format!("procedural-random {name}")),
        calibration: Some(plan.to_string()),
    };
    ModelCard {
        tier: vec![Tier::Abstract, Tier::Medium, Tier::High],
        equations: vec![
            Equation::new(
                "candidate position",
                "p = p_base + d·(cos θ, sin θ), θ ~ U[0, 2π), d ~ U[min_distance, \
                 max_distance]",
            ),
            Equation::new(
                "loop closing",
                "a candidate within min_distance of an existing junction becomes an edge to \
                 the nearest such junction instead of a new one",
            ),
            Equation::new(
                "angle rule",
                "|normalise(θ_new − θ_existing)| >= min_angle at both ends",
            ),
        ],
        parameters: vec![
            Parameter::new("iterations", "-", 150.into(), netgenerate()),
            Parameter::new("min_distance_m", "m", 80.0.into(), netgenerate()),
            Parameter::new("max_distance_m", "m", 250.0.into(), netgenerate()),
            Parameter::new("lanes_per_direction", "-", 2.into(), netgenerate()),
            Parameter::new(
                "lane_width_m",
                "m",
                3.5.into(),
                Source::new(
                    SourceKind::Code,
                    "legacy engine run.py L412-424 lane_width_m, via 04-models.md §1.2 \
                     preset `legacy`",
                ),
            ),
            Parameter::new(
                "seed",
                "-",
                1.into(),
                Source::new(
                    SourceKind::Code,
                    "mapgen.py L244: the `_sN` suffix of a procedural map key, default \
                     seed 1",
                ),
            ),
            todo("min_angle_deg", "deg", 45.0.into(), NETGENERATE_PLAN),
            todo("tries_per_iteration", "-", 50.into(), NETGENERATE_PLAN),
            todo(
                "max_radius_m",
                "m",
                serde_json::Value::Null,
                "This generator's own bound on how far the growth may wander, with no \
                 default because netgenerate has no such option and no source states a \
                 city radius. A scenario that must fit a known area sets it; otherwise the \
                 extent is whatever the draws produced and the report states it.",
            ),
            todo(
                "speed_limit_mps",
                "m/s",
                13.89.into(),
                "13.89 m/s is 50 km/h, a placeholder and not a measurement; take the urban \
                 default from the `maxspeed` distribution of the Phase 2 city bounding \
                 boxes and record the median",
            ),
            todo(
                "cycle_s",
                "s",
                60.0.into(),
                "fit the cycle length and split to the fundamental-diagram targets of \
                 04-models.md §2.9 once the mobility tier can measure junction throughput",
            ),
            todo("amber_s", "s", 3.0.into(), "same study as cycle_s"),
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
            "Every street is two-way with the same number of lanes each way, straight \
             between its two junctions, and the ground is flat at z = 0."
                .to_string(),
            "A junction is wherever two streets meet; the growth never creates a crossing \
             without a junction, because a crossing candidate is rejected outright."
                .to_string(),
            "Turning movements are right from the kerb lane, straight on from every lane, \
             left from the median lane; no turnarounds are generated, which is \
             netgenerate's --no-turnarounds."
                .to_string(),
        ],
        limitations: vec![
            "This is `netgenerate --rand`'s *idea*, not its algorithm: SUMO's own growth \
             rule, its option defaults and therefore its networks are not reproduced, and \
             a world from this generator is not comparable with one from netgenerate."
                .to_string(),
            "The growth is unbounded unless max_radius_m is set, so the extent of a \
             150-iteration network is a draw rather than a parameter."
                .to_string(),
            "Loop closing is the only source of cycles, so a low min_distance_m gives a \
             network that is nearly a tree and routes badly. The connectivity is reported, \
             not controlled."
                .to_string(),
            "No buildings, no crossings, no sidewalks and no RSU sites.".to_string(),
        ],
        ignores: vec![
            "Terrain: every z is 0 until a DEM is attached (see `crate::dem`).".to_string(),
        ],
        sources: vec![netgenerate()],
        validation: Validation::new(ValidationStatus::Unvalidated),
        determinism: Determinism {
            uses_rng: true,
            rng_domains: vec![format!("plugin:{MODEL_ID}")],
        },
        ..ModelCard::new(
            MODEL_ID,
            Family::World,
            MODEL_VERSION,
            "An irregular road network grown from one junction by random attachment: \
             oblique approaches, three-arm junctions, cul-de-sacs and uneven blocks, \
             reproducible from its seed.",
        )
    }
}
