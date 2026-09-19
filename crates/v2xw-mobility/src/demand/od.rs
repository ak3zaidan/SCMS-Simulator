//! `mobility/demand/od-gravity` — where a trip starts and ends (04-models.md §2.4).
//!
//! Two laws, both from §2.4:
//!
//! * **uniform** — origin and destination drawn uniformly from the lanes the class may use.
//! * **gravity** — the destination's weight decays with the number of junction hops from the
//!   origin, `w(h) = exp(−h / scale)`, with `od_gravity_scale` = 2.0 (`code (legacy)`).
//!
//! Two modifiers, also from §2.4:
//!
//! * `boundary_origins` places origins on the network's perimeter, so traffic enters from
//!   the outside rather than appearing in the middle of a street.
//! * in the `rush` profile the destination is the network centre with probability
//!   `m(f) − 0.2` — the commute-to-core bias [`run.py` L1741-1743].
//!
//! # Fixed draw count
//!
//! One call draws exactly three numbers — the origin, the centre-bias coin, the destination
//! — whichever law is in force. A sampler whose draw count depended on the law would shift
//! every later draw in the stream when the law changed, which would make two scenarios that
//! differ only in their OD law incomparable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::{JunctionId, LaneId};
use v2xw_core::math;
use v2xw_core::rng::{RngDomain, RngStream};
use v2xw_world::{ClassMask, LaneKind, World};

use crate::error::{MobError, Result};

/// The model id.
pub const MODEL_ID: &str = "mobility/demand/od-gravity";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The legacy gravity scale, in junction hops (`od_gravity_scale`).
pub const LEGACY_GRAVITY_SCALE: f64 = 2.0;

/// Which destination law applies.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "law", rename_all = "kebab-case")]
pub enum OdLaw {
    /// Uniform over every usable lane.
    #[default]
    Uniform,
    /// Hop-decayed: `w(h) = exp(−h/scale)`.
    Gravity {
        /// The decay scale, in junction hops.
        scale: f64,
    },
}

impl OdLaw {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            OdLaw::Uniform => "uniform",
            OdLaw::Gravity { .. } => "gravity",
        }
    }
}

/// The OD model's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OdParams {
    /// The destination law.
    pub law: OdLaw,
    /// Whether origins are confined to the perimeter.
    pub boundary_origins: bool,
    /// How wide the perimeter band is, as a fraction of the world's shorter side.
    ///
    /// The legacy engine defined the perimeter per topology in `roads.py`; this crate
    /// defines it geometrically, and the fraction is recorded on the card as this crate's
    /// choice rather than as a value read from anywhere.
    pub boundary_margin_fraction: f64,
    /// Whether the `rush` profile's commute-to-core bias applies.
    pub rush_centre_bias: bool,
    /// How many junction hops the gravity BFS explores. An engineering bound: beyond
    /// `6·scale` the weight is under 0.3 % of the nearest lane's and cannot change a draw
    /// that a `f64` can represent meaningfully.
    pub max_hops: usize,
    /// The classes a trip's lanes must admit.
    pub classes: ClassMask,
}

impl Default for OdParams {
    fn default() -> Self {
        Self {
            law: OdLaw::Uniform,
            boundary_origins: false,
            boundary_margin_fraction: 0.05,
            rush_centre_bias: true,
            max_hops: 12,
            classes: ClassMask::MOTOR_TRAFFIC,
        }
    }
}

impl OdParams {
    /// The legacy gravity configuration.
    pub fn legacy_gravity() -> Self {
        Self {
            law: OdLaw::Gravity {
                scale: LEGACY_GRAVITY_SCALE,
            },
            boundary_origins: true,
            ..Self::default()
        }
    }
}

/// Origin-destination sampling over one world.
///
/// Built once per run: the candidate lists, the junction adjacency and the centre lane are
/// pure functions of the world, so they are computed here rather than per trip.
#[derive(Debug, Clone)]
pub struct OdModel {
    params: OdParams,
    origins: Vec<LaneId>,
    destinations: Vec<LaneId>,
    centre: Option<LaneId>,
    /// Junction adjacency, for the gravity BFS.
    adjacency: BTreeMap<JunctionId, Vec<JunctionId>>,
    /// Which junction each destination lane leaves from, for the hop count.
    destination_junction: Vec<JunctionId>,
    card: ModelCard,
}

impl OdModel {
    /// Builds the model over `world`.
    ///
    /// # Errors
    ///
    /// [`MobError::EmptyWorld`] if the world has no lane the class may use.
    pub fn build(world: &World, params: OdParams) -> Result<Self> {
        let usable: Vec<LaneId> = world
            .roads
            .lanes()
            .iter()
            .filter(|l| {
                l.kind != LaneKind::Internal && l.admits(params.classes) && l.kind.is_motorised()
            })
            .map(|l| l.id)
            .collect();
        if usable.is_empty() {
            return Err(MobError::EmptyWorld {
                what: "lane the requested classes may use",
            });
        }
        let origins = if params.boundary_origins {
            let band = params.boundary_margin_fraction
                * (world.bbox.size().x.min(world.bbox.size().y)).max(0.0);
            let on_perimeter: Vec<LaneId> = usable
                .iter()
                .copied()
                .filter(|l| {
                    let lane = world.lane(*l);
                    let p = lane.start();
                    let b = world.bbox;
                    (p.x - b.min.x).abs() <= band
                        || (b.max.x - p.x).abs() <= band
                        || (p.y - b.min.y).abs() <= band
                        || (b.max.y - p.y).abs() <= band
                })
                .collect();
            if on_perimeter.is_empty() {
                usable.clone()
            } else {
                on_perimeter
            }
        } else {
            usable.clone()
        };

        let mut adjacency: BTreeMap<JunctionId, Vec<JunctionId>> = BTreeMap::new();
        for edge in world.roads.edges() {
            if edge.from == edge.to {
                continue;
            }
            adjacency.entry(edge.from).or_default().push(edge.to);
        }
        for list in adjacency.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        let destination_junction = usable
            .iter()
            .map(|l| world.edge(world.lane(*l).edge).from)
            .collect();
        let centre = world
            .nearest_lane_within(world.bbox.center(), f64::INFINITY, Some(params.classes))
            .map(|m| m.pos.lane)
            .filter(|l| world.lane(*l).kind.is_motorised());

        Ok(Self {
            card: card(&params),
            params,
            origins,
            destinations: usable,
            centre,
            adjacency,
            destination_junction,
        })
    }

    /// The parameters in force.
    pub fn params(&self) -> &OdParams {
        &self.params
    }

    /// The candidate origins, in lane-id order.
    pub fn origins(&self) -> &[LaneId] {
        &self.origins
    }

    /// The candidate destinations, in lane-id order.
    pub fn destinations(&self) -> &[LaneId] {
        &self.destinations
    }

    /// The lane nearest the network centre, which is where the rush bias sends trips.
    pub fn centre(&self) -> Option<LaneId> {
        self.centre
    }

    /// Draws an `(origin, destination)` pair. Exactly three draws from `rng`.
    ///
    /// `profile_multiplier` is the demand shape's value at this instant; the commute-to-core
    /// bias fires with probability `m(f) − 0.2`, which is §2.4's rule, so it is zero
    /// outside a rush peak and up to 0.8 at one.
    pub fn draw(
        &self,
        world: &World,
        rng: &mut RngStream,
        profile_multiplier: f64,
    ) -> (LaneId, LaneId) {
        let origin = self.origins[rng.below(self.origins.len() as u64) as usize];
        let centre_coin = rng.f64();
        let centre_probability = if self.params.rush_centre_bias {
            (profile_multiplier - 0.2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let weights = match self.params.law {
            OdLaw::Uniform => None,
            OdLaw::Gravity { scale } => Some(self.gravity_weights(world, origin, scale)),
        };
        let destination = match weights {
            Some(w) if w.iter().any(|x| *x > 0.0) => self.destinations[rng.choose_index(&w)],
            _ => self.destinations[rng.below(self.destinations.len() as u64) as usize],
        };
        let destination = if centre_coin < centre_probability {
            self.centre.unwrap_or(destination)
        } else {
            destination
        };
        (origin, destination)
    }

    /// `w(h) = exp(−h/scale)` for every candidate destination, zero beyond the hop bound.
    fn gravity_weights(&self, world: &World, origin: LaneId, scale: f64) -> Vec<f64> {
        let start = world.edge(world.lane(origin).edge).to;
        let mut hops: BTreeMap<JunctionId, usize> = BTreeMap::new();
        hops.insert(start, 0);
        let mut queue: VecDeque<JunctionId> = VecDeque::from([start]);
        let mut seen: BTreeSet<JunctionId> = BTreeSet::from([start]);
        while let Some(j) = queue.pop_front() {
            let h = hops[&j];
            if h >= self.params.max_hops {
                continue;
            }
            for next in self.adjacency.get(&j).map_or(&[][..], |v| v.as_slice()) {
                if seen.insert(*next) {
                    hops.insert(*next, h + 1);
                    queue.push_back(*next);
                }
            }
        }
        let scale = if scale > 0.0 { scale } else { 1.0 };
        self.destination_junction
            .iter()
            .map(|j| match hops.get(j) {
                Some(h) => math::exp(-(*h as f64) / scale),
                None => 0.0,
            })
            .collect()
    }
}

impl v2xw_core::model::Model for OdModel {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// The model card.
pub fn card(params: &OdParams) -> ModelCard {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L1741-1743 and `roads.py` \
                    `random_trip` (04-models.md §2.4)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Where a trip starts and ends: uniform or hop-decayed (gravity) destinations, \
         optional perimeter origins, and the rush profile's commute-to-core bias.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![v2xw_core::card::Equation {
        name: "gravity weight".to_string(),
        latex_or_text: "w(h) = exp(−h / od_gravity_scale)".to_string(),
        notes: Some("h is the number of junction hops from the origin".to_string()),
    }];
    card.parameters = vec![
        Parameter::new(
            "od_model",
            "-",
            serde_json::json!(params.law.label()),
            legacy.clone(),
        ),
        Parameter::new(
            "od_gravity_scale",
            "hops",
            serde_json::json!(match params.law {
                OdLaw::Gravity { scale } => scale,
                OdLaw::Uniform => LEGACY_GRAVITY_SCALE,
            }),
            legacy.clone(),
        ),
        Parameter::new(
            "boundary_origins",
            "-",
            serde_json::json!(params.boundary_origins),
            legacy.clone(),
        ),
        Parameter::new(
            "rush_centre_bias",
            "1",
            serde_json::json!("probability m(f) − 0.2"),
            legacy,
        ),
        Parameter::new(
            "boundary_margin_fraction",
            "1",
            serde_json::json!(params.boundary_margin_fraction),
            Source {
                kind: SourceKind::Code,
                reference: "v2xw-mobility: this crate's geometric definition of the \
                            perimeter, a band of this fraction of the world's shorter side"
                    .to_string(),
                accessed: None,
                note: Some(
                    "the legacy engine defined the perimeter per topology in `roads.py`; \
                     a geometric band is topology-independent and is recorded here as this \
                     crate's choice"
                        .to_string(),
                ),
            },
        ),
        Parameter::new(
            "max_hops",
            "hops",
            serde_json::json!(params.max_hops),
            Source::new(
                SourceKind::Code,
                "an engineering bound on the gravity BFS: beyond 6·scale the weight is \
                 under 0.3 % of the nearest lane's",
            ),
        ),
        Parameter::new(
            "classes",
            "-",
            serde_json::json!(params.classes.names()),
            Source::new(
                SourceKind::Code,
                "04-models.md §1.1: a lane carries the class mask that may use it",
            ),
        ),
    ];
    card.assumptions = vec![
        "A trip's origin and destination are lanes the class may use and whose kind is for \
         motor traffic, so a car is never given a sidewalk to start on."
            .to_string(),
        "One draw for the origin, one for the centre-bias coin and one for the \
         destination, whichever law is in force."
            .to_string(),
    ];
    card.limitations = vec![
        "Hops are counted on the junction graph in the direction of travel, so a \
         destination reachable only against the flow has weight zero."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Spawn.as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["demand::od::tests::gravity_prefers_nearby_destinations".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::model::Model;
    use v2xw_core::rng::{EntityRef, RngRegistry};
    use v2xw_world::{ImportOptions, procedural::GridParams};

    fn world() -> World {
        v2xw_world::procedural::grid(&GridParams::legacy(), &ImportOptions::default())
            .expect("grid")
    }

    #[test]
    fn every_drawn_lane_is_usable() {
        let w = world();
        let m = OdModel::build(&w, OdParams::default()).expect("built");
        let registry = RngRegistry::new(9);
        let mut rng = registry.ephemeral(RngDomain::Spawn, EntityRef::Global);
        for _ in 0..500 {
            let (o, d) = m.draw(&w, &mut rng, 1.0);
            for lane in [o, d] {
                let l = w.lane(lane);
                assert!(l.kind.is_motorised());
                assert!(l.admits(ClassMask::CAR));
            }
        }
    }

    #[test]
    fn gravity_prefers_nearby_destinations() {
        let w = world();
        let uniform = OdModel::build(&w, OdParams::default()).expect("built");
        let gravity = OdModel::build(
            &w,
            OdParams {
                law: OdLaw::Gravity {
                    scale: LEGACY_GRAVITY_SCALE,
                },
                rush_centre_bias: false,
                ..OdParams::default()
            },
        )
        .expect("built");
        let registry = RngRegistry::new(4);
        // Mean straight-line trip distance: the gravity law must make it shorter.
        let mean_distance = |m: &OdModel, seed_domain| {
            let mut rng = registry.ephemeral(seed_domain, EntityRef::Global);
            let mut total = 0.0;
            let n = 400;
            for _ in 0..n {
                let (o, d) = m.draw(&w, &mut rng, 1.0);
                total += w.lane(o).start().distance_2d(w.lane(d).start());
            }
            total / f64::from(n)
        };
        let u = mean_distance(&uniform, RngDomain::Spawn);
        let g = mean_distance(&gravity, RngDomain::Spawn);
        assert!(g < u, "gravity {g} m is not shorter than uniform {u} m");
    }

    #[test]
    fn the_rush_bias_sends_trips_to_the_centre() {
        let w = world();
        let m = OdModel::build(&w, OdParams::default()).expect("built");
        let centre = m.centre().expect("a centre lane");
        let registry = RngRegistry::new(21);
        let mut rng = registry.ephemeral(RngDomain::Spawn, EntityRef::Global);
        let mut to_centre = 0;
        let n = 2000;
        for _ in 0..n {
            // At the peak, m(f) = 1.0, so the bias fires with probability 0.8.
            let (_, d) = m.draw(&w, &mut rng, 1.0);
            if d == centre {
                to_centre += 1;
            }
        }
        let share = f64::from(to_centre) / f64::from(n);
        assert!((share - 0.8).abs() < 0.05, "centre share {share}");
        // Off-peak (m(f) = 0.2) it never fires.
        let mut off = 0;
        for _ in 0..500 {
            let (_, d) = m.draw(&w, &mut rng, 0.2);
            if d == centre {
                off += 1;
            }
        }
        assert!(
            f64::from(off) / 500.0 < 0.1,
            "off-peak centre share {off}/500"
        );
    }

    #[test]
    fn boundary_origins_are_on_the_perimeter() {
        let w = world();
        let m = OdModel::build(
            &w,
            OdParams {
                boundary_origins: true,
                ..OdParams::default()
            },
        )
        .expect("built");
        let all = OdModel::build(&w, OdParams::default()).expect("built");
        assert!(
            m.origins().len() < all.origins().len(),
            "the perimeter is a strict subset: {} of {}",
            m.origins().len(),
            all.origins().len()
        );
        let band = 0.05 * w.bbox.size().x.min(w.bbox.size().y);
        for o in m.origins() {
            let p = w.lane(*o).start();
            let b = w.bbox;
            let near = (p.x - b.min.x).abs() <= band
                || (b.max.x - p.x).abs() <= band
                || (p.y - b.min.y).abs() <= band
                || (b.max.y - p.y).abs() <= band;
            assert!(near, "origin {o} is not on the perimeter");
        }
    }

    #[test]
    fn the_draw_count_does_not_depend_on_the_law() {
        // Two models differing only in their law consume the same number of draws, so a
        // scenario that switches the law keeps every later draw in the stream aligned.
        let w = world();
        let registry = RngRegistry::new(77);
        let uniform = OdModel::build(
            &w,
            OdParams {
                rush_centre_bias: false,
                ..OdParams::default()
            },
        )
        .expect("built");
        let gravity = OdModel::build(
            &w,
            OdParams {
                law: OdLaw::Gravity { scale: 2.0 },
                rush_centre_bias: false,
                ..OdParams::default()
            },
        )
        .expect("built");
        let mut a = registry.ephemeral(RngDomain::Spawn, EntityRef::Global);
        let mut b = registry.ephemeral(RngDomain::Spawn, EntityRef::Global);
        for _ in 0..50 {
            uniform.draw(&w, &mut a, 1.0);
            gravity.draw(&w, &mut b, 1.0);
        }
        assert_eq!(a.word_pos(), b.word_pos(), "the streams stay aligned");
    }

    #[test]
    fn an_empty_world_is_refused() {
        let w = world();
        let err = OdModel::build(
            &w,
            OdParams {
                classes: ClassMask::RAIL,
                ..OdParams::default()
            },
        )
        .expect_err("no rail lanes");
        assert!(matches!(err, MobError::EmptyWorld { .. }));
    }

    #[test]
    fn the_card_validates() {
        let w = world();
        OdModel::build(&w, OdParams::legacy_gravity())
            .expect("built")
            .card()
            .validate()
            .expect("validates");
    }
}
