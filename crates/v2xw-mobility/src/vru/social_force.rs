//! `vru/pedestrian/social-force` — Helbing and Molnár 1995 (04-models.md §2.5).
//!
//! # The model
//!
//! ```text
//! dv_α/dt = (v0_α·e_α − v_α)/τ  +  Σ_β f_αβ  +  Σ_B f_αB  +  fluctuations
//! f_αβ = −∇_r V_αβ(b),   V_αβ(b) = V0·exp(−b/σ)
//! f_αB = −∇   U_αB(d),   U_αB(d) = U0·exp(−d/R)
//! b    = ½·√( (|r| + |r − y|)² − |y|² ),   y = v_β·Δt·e_β
//! ```
//!
//! `b` is the semi-minor axis of an ellipse whose foci are the other pedestrian's present
//! and next positions: the repulsion is *directed*, because a pedestrian steps out of the
//! way of where the other one is **going**. Its gradient has a closed form,
//! `∇_r b = ((|r| + |r − y|) / (4b))·(r̂ + (r − y)ˆ)`, which is what this module evaluates.
//!
//! **Status of the gradient:** R10 §B11 records the parameter table and the *form* of the
//! model (`dw/dt = F + fluctuations`, with a desired-direction term, pairwise repulsion,
//! border repulsion and optional attraction), not this reduction. The closed form above is
//! the standard one and is transcribed rather than quoted, so it is flagged on the card.
//! [`SocialForceParams::elliptical`] turns it off, which falls back to the isotropic
//! `b = |r|` that needs no reduction at all.
//!
//! # The parameters, all from §2.5
//!
//! | Parameter | Default |
//! |---|---|
//! | desired speed mean, std | 1.34, 0.26 m/s (Gaussian) |
//! | maximum speed | 1.3 × v0 |
//! | relaxation time τ | 0.5 s |
//! | pedestrian-pedestrian `V0`, `σ` | 2.1 m²/s², 0.3 m |
//! | pedestrian-border `U0`, `R` | 10 m²/s², 0.2 m |
//! | step width Δt (elliptical potential) | 2 s |
//! | field of view 2φ | 200° |
//! | behind-view weight c | 0.5 |
//! | vehicle blocking threshold | 10 m (`jmCrossingGap`, R10 §B13) |
//!
//! The fluctuation term has **no cited magnitude** — the paper says "fluctuations" — so its
//! default is zero and it carries a `TODO: calibrate` plan. A pedestrian model with an
//! invented noise term would look more realistic and be less true.
//!
//! # Staying on the pavement
//!
//! The border potential is soft: with a finite step a pedestrian *could* cross a kerb, and
//! at `U0 = 10 m²/s²` and `R = 0.2 m` the force only becomes large within a few
//! decimetres. So the model also **clamps** the lateral offset to the walkable half-width
//! at the end of every step. The clamp is a hard constraint on top of a soft potential,
//! recorded on the card: without it the published invariant "pedestrians walk on sidewalk
//! lanes and crossings" would hold only statistically, and the crate's own test asserts it
//! absolutely.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geom::{Dims, LanePos, Vec3};
use v2xw_core::ids::{ActorId, LaneId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::math;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::Duration;
use v2xw_world::{LaneKind, World};

use crate::classes::VehicleClass;
use crate::ctx::MobCtx;
use crate::error::{MobError, Result};
use crate::snapshot::ActorSnapshot;
use crate::traits::VruMobility;

/// The model id.
pub const MODEL_ID: &str = "vru/pedestrian/social-force";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The vehicle-side blocking threshold, metres (SUMO `jmCrossingGap`, R10 §B13).
pub const CROSSING_GAP_M: f64 = 10.0;

/// The model's parameters (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SocialForceParams {
    /// Mean desired speed, m/s.
    pub desired_speed_mean_mps: f64,
    /// Standard deviation of the desired speed, m/s.
    pub desired_speed_std_mps: f64,
    /// Maximum speed, as a multiple of the drawn desired speed.
    pub max_speed_factor: f64,
    /// Relaxation time `τ`, seconds.
    pub tau_s: f64,
    /// Pedestrian-pedestrian potential `V0`, m²/s².
    pub v0_m2_s2: f64,
    /// Pedestrian-pedestrian decay length `σ`, metres.
    pub sigma_m: f64,
    /// Pedestrian-border potential `U0`, m²/s².
    pub u0_m2_s2: f64,
    /// Pedestrian-border decay length `R`, metres.
    pub r_m: f64,
    /// Step width `Δt` of the elliptical potential, seconds.
    pub step_width_s: f64,
    /// Field of view `2φ`, degrees.
    pub field_of_view_deg: f64,
    /// Weight of an influence from outside the field of view, `c`.
    pub behind_weight: f64,
    /// Whether to use the elliptical potential (see the module documentation).
    pub elliptical: bool,
    /// How far to look for other pedestrians and vehicles, metres. Not a model parameter:
    /// the potentials are exponential with a decay length of 0.3 m, so the force beyond a
    /// few metres is numerically zero, and this is where the query stops paying for it.
    pub interaction_radius_m: f64,
    /// The vehicle-side blocking threshold on a crossing, metres.
    pub crossing_gap_m: f64,
    /// Standard deviation of the fluctuation acceleration, m/s². **`TODO: calibrate`** —
    /// zero by default.
    pub fluctuation_mps2: f64,
}

impl Default for SocialForceParams {
    fn default() -> Self {
        Self {
            desired_speed_mean_mps: 1.34,
            desired_speed_std_mps: 0.26,
            max_speed_factor: 1.3,
            tau_s: 0.5,
            v0_m2_s2: 2.1,
            sigma_m: 0.3,
            u0_m2_s2: 10.0,
            r_m: 0.2,
            step_width_s: 2.0,
            field_of_view_deg: 200.0,
            behind_weight: 0.5,
            elliptical: true,
            interaction_radius_m: 10.0,
            crossing_gap_m: CROSSING_GAP_M,
            fluctuation_mps2: 0.0,
        }
    }
}

/// One pedestrian.
#[derive(Debug, Clone, PartialEq)]
pub struct Pedestrian {
    /// Which actor.
    pub actor: ActorId,
    /// The walkable lane it is on.
    pub lane: LaneId,
    /// Arc length along that lane, metres.
    pub s_m: f64,
    /// Lateral offset from the centreline, metres, positive to the left of travel.
    pub lateral_m: f64,
    /// Velocity, m/s, in world coordinates.
    pub vel: Vec3,
    /// This pedestrian's drawn desired speed, m/s.
    pub desired_speed_mps: f64,
    /// The walkable lanes it intends to use, in order.
    pub route: Vec<LaneId>,
    /// Where it is on that route.
    pub route_index: usize,
    /// True once it has walked off the end of its route.
    pub arrived: bool,
}

impl Pedestrian {
    /// Its world position, given the world.
    pub fn position(&self, world: &World) -> Vec3 {
        world
            .try_lane(self.lane)
            .map(|l| l.offset_point(self.s_m, self.lateral_m))
            .unwrap_or(Vec3::ZERO)
    }
}

/// The social-force pedestrian model.
#[derive(Debug, Clone)]
pub struct SocialForce {
    params: SocialForceParams,
    people: BTreeMap<ActorId, Pedestrian>,
    card: ModelCard,
}

impl Default for SocialForce {
    fn default() -> Self {
        SocialForce::new(SocialForceParams::default())
    }
}

impl SocialForce {
    /// The model with the given parameters.
    pub fn new(params: SocialForceParams) -> Self {
        Self {
            card: card(&params),
            params,
            people: BTreeMap::new(),
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &SocialForceParams {
        &self.params
    }

    /// True if `lane` is somewhere a pedestrian may walk.
    pub fn is_walkable(world: &World, lane: LaneId) -> bool {
        world.try_lane(lane).is_some_and(|l| {
            matches!(l.kind, LaneKind::Sidewalk | LaneKind::Crossing)
                && l.admits(v2xw_world::ClassMask::PEDESTRIAN)
        })
    }

    /// Puts a pedestrian on `route` at `s_m` along its first lane.
    ///
    /// The desired speed is drawn from this actor's own `desired-speed` stream, so it does
    /// not depend on how many other pedestrians were spawned first.
    ///
    /// # Errors
    ///
    /// [`MobError::LaneNotAdmitted`] if any lane of the route is not walkable, and
    /// [`MobError::NoSuchLane`] if the route is empty or names a lane the world lacks.
    pub fn spawn(
        &mut self,
        ctx: &mut dyn MobCtx,
        actor: ActorId,
        route: Vec<LaneId>,
        s_m: f64,
    ) -> Result<()> {
        let world = ctx.world();
        let first = *route.first().ok_or(MobError::EmptyWorld {
            what: "lane in the pedestrian's route",
        })?;
        for lane in &route {
            let l = world
                .try_lane(*lane)
                .ok_or(MobError::NoSuchLane { lane: *lane })?;
            if !Self::is_walkable(world, *lane) {
                return Err(MobError::LaneNotAdmitted {
                    lane: *lane,
                    kind: l.kind.wire_name(),
                    classes: "pedestrian".to_string(),
                });
            }
        }
        let desired = {
            let mut rng = ctx.rng(RngDomain::DesiredSpeed, EntityRef::Actor(actor));
            rng.normal(
                self.params.desired_speed_mean_mps,
                self.params.desired_speed_std_mps,
            )
            .max(0.1)
        };
        self.people.insert(
            actor,
            Pedestrian {
                actor,
                lane: first,
                s_m,
                lateral_m: 0.0,
                vel: Vec3::ZERO,
                desired_speed_mps: desired,
                route,
                route_index: 0,
                arrived: false,
            },
        );
        Ok(())
    }

    /// Removes a pedestrian.
    pub fn despawn(&mut self, actor: ActorId) -> Option<Pedestrian> {
        self.people.remove(&actor)
    }

    /// Every pedestrian, in actor-id order.
    pub fn people(&self) -> impl Iterator<Item = &Pedestrian> + '_ {
        self.people.values()
    }

    /// One pedestrian.
    pub fn get(&self, actor: ActorId) -> Option<&Pedestrian> {
        self.people.get(&actor)
    }

    /// How many pedestrians the model holds.
    pub fn len(&self) -> usize {
        self.people.len()
    }

    /// True if it holds none.
    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    /// The field-of-view weight for an influence `f` on a pedestrian facing `e`
    /// (§2.5: `w = 1` inside the field of view, `c` outside it).
    pub fn view_weight(&self, facing: Vec3, force: Vec3) -> f64 {
        let magnitude = force.norm_2d();
        if magnitude <= 0.0 {
            return 1.0;
        }
        let half_angle = 0.5 * self.params.field_of_view_deg * core::f64::consts::PI / 180.0;
        let cos_phi = math::cos(half_angle);
        if facing.dot(force) >= magnitude * cos_phi {
            1.0
        } else {
            self.params.behind_weight
        }
    }

    /// The repulsion one pedestrian feels from another (§2.5).
    ///
    /// `r` is the vector from the other pedestrian to this one; `other_vel` is the other's
    /// velocity, which the elliptical potential uses to look one step ahead.
    pub fn pedestrian_repulsion(&self, r: Vec3, other_vel: Vec3) -> Vec3 {
        let r_norm = r.norm_2d();
        if r_norm <= 1e-9 {
            return Vec3::ZERO;
        }
        if !self.params.elliptical {
            let magnitude = (self.params.v0_m2_s2 / self.params.sigma_m)
                * math::exp(-r_norm / self.params.sigma_m);
            return Vec3::new_2d(r.x / r_norm * magnitude, r.y / r_norm * magnitude);
        }
        // The elliptical potential: the other pedestrian's next position is a second focus.
        let y = Vec3::new_2d(
            other_vel.x * self.params.step_width_s,
            other_vel.y * self.params.step_width_s,
        );
        let r_minus_y = Vec3::new_2d(r.x - y.x, r.y - y.y);
        let r_minus_y_norm = r_minus_y.norm_2d();
        let y_norm = y.norm_2d();
        let sum = r_norm + r_minus_y_norm;
        let b_squared = 0.25 * (sum * sum - y_norm * y_norm);
        // Two numerical guards, both for the same degenerate case: the other pedestrian's
        // *next* position coincides with ours, so the ellipse collapses (`b → 0`) and the
        // second unit vector is undefined. The force there is maximal, not zero, so `b` is
        // floored at a millimetre — which caps the repulsion at
        // `(V0/σ)·(|r| + |r−y|)/(4·1 mm)` — and the collapsed direction falls back on the
        // separation itself.
        const FLOOR_M: f64 = 1e-3;
        let b = math::sqrt(b_squared.max(0.0)).max(FLOOR_M);
        let (dir_x, dir_y) = if r_minus_y_norm > FLOOR_M {
            (
                r.x / r_norm + r_minus_y.x / r_minus_y_norm,
                r.y / r_norm + r_minus_y.y / r_minus_y_norm,
            )
        } else {
            (2.0 * r.x / r_norm, 2.0 * r.y / r_norm)
        };
        // −dV/db = (V0/σ)·exp(−b/σ);  ∇_r b = (|r| + |r−y|)/(4b)·(r̂ + (r−y)ˆ).
        let dv_db =
            (self.params.v0_m2_s2 / self.params.sigma_m) * math::exp(-b / self.params.sigma_m);
        let scale = dv_db * sum / (4.0 * b);
        Vec3::new_2d(scale * dir_x, scale * dir_y)
    }

    /// The repulsion a border at distance `d` exerts, magnitude only (§2.5).
    pub fn border_repulsion_magnitude(&self, d_m: f64) -> f64 {
        (self.params.u0_m2_s2 / self.params.r_m) * math::exp(-d_m.max(0.0) / self.params.r_m)
    }
}

impl v2xw_core::model::Model for SocialForce {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl VruMobility for SocialForce {
    fn step(
        &mut self,
        ctx: &mut dyn MobCtx,
        dt: Duration,
        vehicles: &ActorSnapshot,
    ) -> Vec<(ActorId, Kinematics)> {
        let dt_s = dt.as_secs_f64();
        if dt_s <= 0.0 {
            return Vec::new();
        }
        let now = ctx.now();
        // The start-of-step positions of every pedestrian: the same Jacobi discipline the
        // vehicles use, so one pedestrian's move cannot depend on another's having moved.
        let frozen: Vec<(ActorId, Vec3, Vec3)> = {
            let world = ctx.world();
            self.people
                .values()
                .map(|p| (p.actor, p.position(world), p.vel))
                .collect()
        };
        let fluctuation = self.params.fluctuation_mps2;
        let mut out: Vec<(ActorId, Kinematics)> = Vec::with_capacity(self.people.len());
        let actors: Vec<ActorId> = self.people.keys().copied().collect();
        for actor in actors {
            let noise = if fluctuation > 0.0 {
                let mut rng = ctx.rng(RngDomain::plugin(MODEL_ID), EntityRef::Actor(actor));
                Vec3::new_2d(rng.normal(0.0, fluctuation), rng.normal(0.0, fluctuation))
            } else {
                Vec3::ZERO
            };
            let world = ctx.world();
            let Some(person) = self.people.get(&actor) else {
                continue;
            };
            if person.arrived {
                out.push((actor, self.kinematics_of(person, world, now)));
                continue;
            }
            let Some(lane) = world.try_lane(person.lane) else {
                continue;
            };
            let position = lane.offset_point(person.s_m, person.lateral_m);
            let heading = lane.heading_at(person.s_m);
            let (sin_h, cos_h) = math::sin_cos(heading);
            let forward = Vec3::new_2d(cos_h, sin_h);

            // --- the desired-direction term -------------------------------
            // A pedestrian waiting at a kerb has a desired speed of zero: the crossing is
            // blocked while a vehicle is within `crossing_gap_m` of it.
            let blocked = lane.kind == LaneKind::Crossing
                || self.next_lane(person).is_some_and(|next| {
                    world
                        .try_lane(next)
                        .is_some_and(|l| l.kind == LaneKind::Crossing)
                });
            let waiting = blocked && self.vehicle_within(vehicles, position);
            let desired_speed = if waiting {
                0.0
            } else {
                person.desired_speed_mps
            };
            let mut accel = Vec3::new_2d(
                (desired_speed * forward.x - person.vel.x) / self.params.tau_s,
                (desired_speed * forward.y - person.vel.y) / self.params.tau_s,
            );

            // --- pairwise repulsion ---------------------------------------
            for (other, other_pos, other_vel) in &frozen {
                if *other == actor {
                    continue;
                }
                let r = Vec3::new_2d(position.x - other_pos.x, position.y - other_pos.y);
                if r.norm_2d() > self.params.interaction_radius_m {
                    continue;
                }
                let f = self.pedestrian_repulsion(r, *other_vel);
                let w = self.view_weight(forward, f);
                accel = Vec3::new_2d(accel.x + w * f.x, accel.y + w * f.y);
            }

            // --- borders: the kerbs of this lane --------------------------
            let half_width = 0.5 * lane.width_m;
            let body = 0.5 * VehicleClass::Pedestrian.spec().width_m;
            let walkable_half = (half_width - body).max(0.0);
            let left_distance = (walkable_half - person.lateral_m).max(0.0);
            let right_distance = (walkable_half + person.lateral_m).max(0.0);
            let left_normal = Vec3::new_2d(-forward.y, forward.x);
            let left_force = self.border_repulsion_magnitude(left_distance);
            let right_force = self.border_repulsion_magnitude(right_distance);
            accel = Vec3::new_2d(
                accel.x - left_normal.x * left_force + left_normal.x * right_force,
                accel.y - left_normal.y * left_force + left_normal.y * right_force,
            );

            // --- vehicles as borders --------------------------------------
            for vehicle in vehicles.actors_within(position, self.params.interaction_radius_m) {
                let Some(state) = vehicles.kinematics(vehicle) else {
                    continue;
                };
                let r = Vec3::new_2d(position.x - state.pos.x, position.y - state.pos.y);
                let d = r.norm_2d();
                if d <= 1e-9 {
                    continue;
                }
                let magnitude = self.border_repulsion_magnitude(d);
                accel = Vec3::new_2d(accel.x + r.x / d * magnitude, accel.y + r.y / d * magnitude);
            }
            accel = Vec3::new_2d(accel.x + noise.x, accel.y + noise.y);

            // --- integrate -------------------------------------------------
            let mut velocity =
                Vec3::new_2d(person.vel.x + accel.x * dt_s, person.vel.y + accel.y * dt_s);
            let max_speed = self.params.max_speed_factor * person.desired_speed_mps;
            let speed = velocity.norm_2d();
            if speed > max_speed && speed > 0.0 {
                velocity = Vec3::new_2d(
                    velocity.x * max_speed / speed,
                    velocity.y * max_speed / speed,
                );
            }
            let moved = Vec3::new_2d(
                position.x + velocity.x * dt_s,
                position.y + velocity.y * dt_s,
            );

            // --- put it back on its lane -----------------------------------
            let projection = lane.project_point(moved);
            let mut s_m = projection.s_m;
            let mut lateral_m = projection.d_m.clamp(-walkable_half, walkable_half);
            let mut current = person.lane;
            let mut route_index = person.route_index;
            let mut arrived = false;
            if s_m >= lane.length_m {
                match self.next_lane(person) {
                    Some(next) if Self::is_walkable(world, next) => {
                        let overshoot = s_m - lane.length_m;
                        current = next;
                        route_index += 1;
                        s_m = overshoot.min(world.lane(next).length_m);
                        lateral_m = 0.0;
                    }
                    _ => {
                        s_m = lane.length_m;
                        arrived = true;
                    }
                }
            } else if s_m < 0.0 {
                s_m = 0.0;
            }
            let person = self.people.get_mut(&actor).expect("present above");
            person.lane = current;
            person.s_m = s_m;
            person.lateral_m = lateral_m;
            person.vel = velocity;
            person.route_index = route_index;
            person.arrived = arrived;
            let person = &self.people[&actor];
            let world = ctx.world();
            out.push((actor, self.kinematics_of(person, world, now)));
        }
        out.sort_by_key(|(a, _)| *a);
        out
    }
}

impl SocialForce {
    /// The lane after the pedestrian's current one, if its route continues.
    fn next_lane(&self, person: &Pedestrian) -> Option<LaneId> {
        person.route.get(person.route_index + 1).copied()
    }

    /// True if a vehicle is within the blocking threshold of `position`.
    fn vehicle_within(&self, vehicles: &ActorSnapshot, position: Vec3) -> bool {
        !vehicles
            .actors_within(position, self.params.crossing_gap_m)
            .is_empty()
    }

    /// The published kinematics of one pedestrian.
    fn kinematics_of(
        &self,
        person: &Pedestrian,
        world: &World,
        t: v2xw_core::time::SimTime,
    ) -> Kinematics {
        let position = person.position(world);
        let heading = if person.vel.norm_2d() > 1e-6 {
            person.vel.heading_2d()
        } else {
            world
                .try_lane(person.lane)
                .map(|l| l.heading_at(person.s_m))
                .unwrap_or(0.0)
        };
        Kinematics {
            t,
            pos: position,
            vel: person.vel,
            acc: Vec3::ZERO,
            heading_rad: heading,
            yaw_rate_rad_s: 0.0,
            lane: Some(LanePos::new(person.lane, person.s_m, person.lateral_m)),
            dims: Dims::new(
                VehicleClass::Pedestrian.spec().length_m,
                VehicleClass::Pedestrian.spec().width_m,
                VehicleClass::Pedestrian.spec().height_m,
            ),
        }
    }
}

/// The model card.
pub fn card(params: &SocialForceParams) -> ModelCard {
    let helbing = Source {
        kind: SourceKind::Paper,
        reference: "Helbing & Molnár, \"Social force model for pedestrian dynamics\", \
                    Phys. Rev. E 51, 4282 (1995); arXiv:cond-mat/9805244 [R10 §B11]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let sumo = Source {
        kind: SourceKind::Code,
        reference: "SUMO `jmCrossingGap` default 10 m [R10 §B6, §B13]".to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Vru,
        MODEL_VERSION,
        "Pedestrians by the Helbing and Molnár force model: a relaxation towards the \
         desired direction, an elliptical pairwise repulsion that looks one step ahead, a \
         border repulsion from the kerbs of the walkable lane, and vehicles as borders \
         with the SUMO crossing-gap threshold. Pedestrians walk the world's sidewalk and \
         crossing lanes.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation {
            name: "acceleration".to_string(),
            latex_or_text: "dv/dt = (v0·e − v)/τ + Σ f_αβ + Σ f_αB + fluctuations".to_string(),
            notes: None,
        },
        Equation {
            name: "pedestrian repulsion".to_string(),
            latex_or_text: "f_αβ = −∇_r V(b),  V(b) = V0·exp(−b/σ),  \
                            b = ½√((|r| + |r − y|)² − |y|²),  y = v_β·Δt·e_β"
                .to_string(),
            notes: Some(
                "the closed-form gradient ∇_r b = ((|r| + |r−y|)/(4b))·(r̂ + (r−y)ˆ) is the \
                 standard reduction and is **transcribed, not quoted**: R10 §B11 records \
                 the parameter table and the model's form, not this expression. \
                 `elliptical = false` falls back to the isotropic b = |r|."
                    .to_string(),
            ),
        },
        Equation {
            name: "border repulsion".to_string(),
            latex_or_text: "f_αB = (U0/R)·exp(−d/R) away from the border".to_string(),
            notes: None,
        },
        Equation {
            name: "field of view".to_string(),
            latex_or_text: "w = 1 if e·f ≥ |f|·cos(φ), else c".to_string(),
            notes: Some("2φ = 200°, c = 0.5".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "desired_speed",
            "m/s",
            serde_json::json!({
                "mean": params.desired_speed_mean_mps,
                "std": params.desired_speed_std_mps,
            }),
            Source {
                kind: SourceKind::Paper,
                reference: "Helbing & Molnár 1995, citing Henderson 1971/1974 [R10 §B11]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
        ),
        Parameter::new(
            "max_speed_factor",
            "1",
            serde_json::json!(params.max_speed_factor),
            helbing.clone(),
        ),
        Parameter::new("tau", "s", serde_json::json!(params.tau_s), helbing.clone()),
        Parameter::new(
            "V0",
            "m²/s²",
            serde_json::json!(params.v0_m2_s2),
            helbing.clone(),
        ),
        Parameter::new(
            "sigma",
            "m",
            serde_json::json!(params.sigma_m),
            helbing.clone(),
        ),
        Parameter::new(
            "U0",
            "m²/s²",
            serde_json::json!(params.u0_m2_s2),
            helbing.clone(),
        ),
        Parameter::new("R", "m", serde_json::json!(params.r_m), helbing.clone()),
        Parameter::new(
            "step_width",
            "s",
            serde_json::json!(params.step_width_s),
            helbing.clone(),
        ),
        Parameter::new(
            "field_of_view",
            "deg",
            serde_json::json!(params.field_of_view_deg),
            helbing.clone(),
        ),
        Parameter::new(
            "behind_weight",
            "1",
            serde_json::json!(params.behind_weight),
            helbing.clone(),
        ),
        Parameter::new(
            "jmCrossingGap",
            "m",
            serde_json::json!(params.crossing_gap_m),
            sumo,
        ),
        Parameter::new(
            "elliptical",
            "-",
            serde_json::json!(params.elliptical),
            helbing.clone(),
        ),
        Parameter::new(
            "interaction_radius",
            "m",
            serde_json::json!(params.interaction_radius_m),
            Source::new(
                SourceKind::Code,
                "an engineering bound: with σ = 0.3 m the repulsion beyond a few metres is \
                 numerically zero",
            ),
        ),
        Parameter {
            name: "fluctuation".to_string(),
            unit: "m/s²".to_string(),
            default: serde_json::json!(params.fluctuation_mps2),
            range: None,
            source: Source::todo_calibrate(
                "Helbing & Molnár write \"+ fluctuations\" and R10 §B11 records no magnitude",
            ),
            calibration: Some(
                "Plan: fit the fluctuation magnitude to a measured speed-variance or \
                 lane-formation statistic (the paper's own N(W) ≈ 0.36·W + 0.59 lane count \
                 on a 10 m walkway is the check R10 §B11 records). Zero until then, so no \
                 invented noise term shapes a trajectory."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "Pedestrians walk sidewalk and crossing lanes of the world; the lane's own width \
         gives the two borders."
            .to_string(),
        "The lateral offset is **clamped** to the walkable half-width at the end of every \
         step. The border potential is soft, so a finite step could otherwise cross a \
         kerb; the clamp makes \"pedestrians stay on walkable lanes\" an invariant rather \
         than a tendency."
            .to_string(),
        "A pedestrian on or about to enter a crossing waits while any vehicle is within \
         the crossing gap; the wait is expressed as a desired speed of zero, so the \
         relaxation term brings it smoothly to a halt."
            .to_string(),
        "Every pedestrian reads the start-of-step positions of the others and of the \
         vehicles, so the update is the same Jacobi update the vehicles use."
            .to_string(),
    ];
    card.limitations = vec![
        "No group behaviour and no jam state (§2.5 ignores both).".to_string(),
        "The attraction term of the 1995 model (shop windows, companions) is not \
         implemented: it is optional in the paper and has no cited parameters."
            .to_string(),
    ];
    card.ignores = vec![
        "Group behaviour, jam states and SUMO's stripe discretisation (medium relative to \
         high, 04-models.md §2.5)."
            .to_string(),
    ];
    card.sources = vec![helbing];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![
            RngDomain::DesiredSpeed.as_str().to_string(),
            RngDomain::plugin(MODEL_ID).as_str().to_string(),
        ],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "vru::social_force::tests::pedestrians_stay_on_walkable_lanes".to_string(),
            "vru::social_force::tests::two_pedestrians_repel_each_other".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MobilityCtx;
    use crate::worlds::{RingParams, ring};
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_MS;
    use v2xw_world::ClassMask;

    /// A ring whose single lane is a sidewalk, which is the smallest world a pedestrian can
    /// walk in circles on.
    fn pavement_ring() -> (World, Vec<LaneId>) {
        let base = ring(&RingParams {
            circumference_m: 400.0,
            segments: 4,
            lane_width_m: 4.0,
            ..RingParams::default()
        })
        .expect("a ring");
        let w = crate::worlds::rebuild(&base, |lanes, _| {
            for lane in lanes.iter_mut() {
                lane.kind = LaneKind::Sidewalk;
                lane.allowed = ClassMask::PEDESTRIAN;
            }
        })
        .expect("a pavement ring");
        let cycle = crate::worlds::ring_cycle(&w, 0);
        (w, cycle)
    }

    #[test]
    fn the_parameters_are_the_document_values() {
        let p = SocialForceParams::default();
        assert_eq!(p.desired_speed_mean_mps, 1.34);
        assert_eq!(p.desired_speed_std_mps, 0.26);
        assert_eq!(p.max_speed_factor, 1.3);
        assert_eq!(p.tau_s, 0.5);
        assert_eq!(p.v0_m2_s2, 2.1);
        assert_eq!(p.sigma_m, 0.3);
        assert_eq!(p.u0_m2_s2, 10.0);
        assert_eq!(p.r_m, 0.2);
        assert_eq!(p.step_width_s, 2.0);
        assert_eq!(p.field_of_view_deg, 200.0);
        assert_eq!(p.behind_weight, 0.5);
        assert_eq!(p.crossing_gap_m, 10.0);
        assert_eq!(p.fluctuation_mps2, 0.0, "no invented noise");
    }

    #[test]
    fn a_pedestrian_walks_at_its_desired_speed() {
        let (w, cycle) = pavement_ring();
        let rng = RngRegistry::new(3);
        let mut m = SocialForce::default();
        {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.spawn(&mut ctx, ActorId::new(1), cycle.clone(), 5.0)
                .expect("spawned");
        }
        let desired = m.get(ActorId::new(1)).unwrap().desired_speed_mps;
        let snapshot = ActorSnapshot::new(0, 50.0);
        let dt = Duration::from_millis(100);
        for k in 0..100u64 {
            let mut ctx = MobilityCtx::new(k * 100 * NS_PER_MS, &w, &rng);
            m.step(&mut ctx, dt, &snapshot);
        }
        // After ten seconds it is walking at its desired speed, to a per cent or so: the
        // relaxation time is 0.5 s, so twenty of them have passed.
        let speed = m.get(ActorId::new(1)).unwrap().vel.norm_2d();
        assert!(
            (speed - desired).abs() / desired < 0.05,
            "{speed} against {desired}"
        );
        // And it has covered about desired × 10 s of arc.
        let person = m.get(ActorId::new(1)).unwrap();
        let travelled = person.route_index as f64 * 100.0 + person.s_m - 5.0;
        assert!(
            (travelled - desired * 10.0).abs() < 2.0,
            "travelled {travelled} m against {} m",
            desired * 10.0
        );
    }

    #[test]
    fn pedestrians_stay_on_walkable_lanes() {
        // The required invariant: whatever the forces do, a pedestrian is always on a
        // walkable lane and inside its width.
        let (w, cycle) = pavement_ring();
        let rng = RngRegistry::new(11);
        let mut m = SocialForce::new(SocialForceParams {
            // A large fluctuation, to push hard against the kerbs.
            fluctuation_mps2: 5.0,
            ..SocialForceParams::default()
        });
        {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            for id in 1..=12u32 {
                m.spawn(
                    &mut ctx,
                    ActorId::new(id),
                    cycle.clone(),
                    f64::from(id) * 2.0,
                )
                .expect("spawned");
            }
        }
        let snapshot = ActorSnapshot::new(0, 50.0);
        let dt = Duration::from_millis(100);
        for k in 0..600u64 {
            let mut ctx = MobilityCtx::new(k * 100 * NS_PER_MS, &w, &rng);
            let states = m.step(&mut ctx, dt, &snapshot);
            for (actor, k) in states {
                let person = m.get(actor).expect("still here");
                assert!(
                    SocialForce::is_walkable(&w, person.lane),
                    "{actor} left the pavement onto {:?}",
                    w.lane(person.lane).kind
                );
                let half = 0.5 * w.lane(person.lane).width_m;
                assert!(
                    person.lateral_m.abs() <= half + 1e-9,
                    "{actor} is {} m off a {half} m half-width lane",
                    person.lateral_m
                );
                // And the published kinematics agree with the lane position.
                let lane_pos = k.lane.expect("a pedestrian is on a lane");
                assert_eq!(lane_pos.lane, person.lane);
                assert!((lane_pos.s_m - person.s_m).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn two_pedestrians_repel_each_other() {
        let m = SocialForce::default();
        // Head on, one metre apart: the force pushes them apart along the separation.
        let r = Vec3::new_2d(1.0, 0.0);
        let f = m.pedestrian_repulsion(r, Vec3::ZERO);
        assert!(f.x > 0.0, "the force is away from the other: {f:?}");
        assert!(f.y.abs() < 1e-12);
        // It decays exponentially: ten centimetres closer is markedly stronger.
        let near = m.pedestrian_repulsion(Vec3::new_2d(0.5, 0.0), Vec3::ZERO);
        assert!(near.x > f.x * 2.0, "{} against {}", near.x, f.x);
        // And the isotropic form matches the closed-form exponential exactly.
        let iso = SocialForce::new(SocialForceParams {
            elliptical: false,
            ..SocialForceParams::default()
        });
        let want = (2.1 / 0.3) * math::exp(-1.0 / 0.3);
        let got = iso.pedestrian_repulsion(r, Vec3::ZERO);
        assert!((got.x - want).abs() < 1e-9, "{} against {want}", got.x);
        // With a stationary neighbour the elliptical form reduces to the isotropic one.
        assert!((f.x - want).abs() < 1e-9, "{} against {want}", f.x);
    }

    #[test]
    fn the_elliptical_potential_looks_ahead() {
        let m = SocialForce::default();
        // A neighbour walking *towards* us repels more than a stationary one at the same
        // distance, because its next position is closer.
        let r = Vec3::new_2d(2.0, 0.0);
        let still = m.pedestrian_repulsion(r, Vec3::ZERO).norm_2d();
        let closing = m.pedestrian_repulsion(r, Vec3::new_2d(0.5, 0.0)).norm_2d();
        assert!(closing > still, "closing {closing} against still {still}");
        // And a neighbour walking away repels less.
        let leaving = m.pedestrian_repulsion(r, Vec3::new_2d(-0.5, 0.0)).norm_2d();
        assert!(leaving < still, "leaving {leaving} against still {still}");
        // The degenerate case — the neighbour's next position is exactly ours — is finite
        // and at least as strong as the stationary case, not zero.
        let collapsed = m.pedestrian_repulsion(r, Vec3::new_2d(1.0, 0.0)).norm_2d();
        assert!(collapsed.is_finite() && collapsed > still, "{collapsed}");
    }

    #[test]
    fn the_border_force_keeps_a_pedestrian_off_the_kerb() {
        let m = SocialForce::default();
        // At the kerb the force is U0/R = 50 m/s²; two decay lengths away it is e⁻² of that.
        assert!((m.border_repulsion_magnitude(0.0) - 50.0).abs() < 1e-9);
        let far = m.border_repulsion_magnitude(0.4);
        assert!((far - 50.0 * math::exp(-2.0)).abs() < 1e-9);
        assert!(m.border_repulsion_magnitude(2.0) < 0.01);
    }

    #[test]
    fn the_field_of_view_halves_an_influence_from_behind() {
        let m = SocialForce::default();
        let facing = Vec3::new_2d(1.0, 0.0);
        // 200° of view: anything within 100° of straight ahead is seen.
        assert_eq!(m.view_weight(facing, Vec3::new_2d(1.0, 0.0)), 1.0);
        assert_eq!(m.view_weight(facing, Vec3::new_2d(0.0, 1.0)), 1.0);
        assert_eq!(m.view_weight(facing, Vec3::new_2d(-1.0, 0.0)), 0.5);
    }

    #[test]
    fn a_pedestrian_waits_for_a_vehicle_at_a_crossing() {
        // A crossing lane and a vehicle within the crossing gap: the pedestrian's desired
        // speed goes to zero and it slows to a halt.
        let base = ring(&RingParams {
            circumference_m: 200.0,
            segments: 4,
            lane_width_m: 4.0,
            ..RingParams::default()
        })
        .expect("a ring");
        let w = crate::worlds::rebuild(&base, |lanes, _| {
            for lane in lanes.iter_mut() {
                lane.kind = LaneKind::Crossing;
                lane.allowed = ClassMask::PEDESTRIAN;
            }
        })
        .expect("a crossing ring");
        let cycle = crate::worlds::ring_cycle(&w, 0);
        let rng = RngRegistry::new(5);
        let mut m = SocialForce::default();
        {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            m.spawn(&mut ctx, ActorId::new(1), cycle, 2.0)
                .expect("spawned");
        }
        // Get it walking first, with no vehicle about.
        let empty = ActorSnapshot::new(0, 50.0);
        let dt = Duration::from_millis(100);
        for k in 0..30u64 {
            let mut ctx = MobilityCtx::new(k * 100 * NS_PER_MS, &w, &rng);
            m.step(&mut ctx, dt, &empty);
        }
        let walking = m.get(ActorId::new(1)).unwrap().vel.norm_2d();
        assert!(walking > 0.5, "it was walking: {walking}");
        // Now put a vehicle five metres away.
        let position = m.get(ActorId::new(1)).unwrap().position(&w);
        let mut snapshot = ActorSnapshot::new(0, 50.0);
        let vehicle = crate::views::VehicleView {
            actor: ActorId::new(99),
            class: VehicleClass::Passenger,
            lane: cycle_first(&w),
            lane_index: 0,
            s_m: 0.0,
            lateral_m: 0.0,
            speed_mps: 0.0,
            accel_mps2: 0.0,
            heading_rad: 0.0,
            dims: VehicleClass::Passenger.dims(),
            driver: crate::carfollowing::IdmPreset::Kesting2010.profile(VehicleClass::Passenger),
        };
        snapshot.push(
            vehicle,
            Kinematics::at_rest(0, Vec3::new(position.x + 5.0, position.y, 0.0)),
        );
        snapshot.sort();
        for k in 30..60u64 {
            let mut ctx = MobilityCtx::new(k * 100 * NS_PER_MS, &w, &rng);
            m.step(&mut ctx, dt, &snapshot);
        }
        let waiting = m.get(ActorId::new(1)).unwrap().vel.norm_2d();
        assert!(
            waiting < walking / 2.0,
            "it should slow: {waiting} from {walking}"
        );
    }

    fn cycle_first(w: &World) -> LaneId {
        w.roads.lanes()[0].id
    }

    #[test]
    fn a_pedestrian_may_not_be_put_on_a_road() {
        let w = ring(&RingParams::default()).expect("a ring");
        let rng = RngRegistry::new(1);
        let mut m = SocialForce::default();
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        let err = m
            .spawn(&mut ctx, ActorId::new(1), vec![w.roads.lanes()[0].id], 0.0)
            .expect_err("a driving lane is not walkable");
        assert!(matches!(err, MobError::LaneNotAdmitted { .. }));
        assert!(m.is_empty());
    }

    #[test]
    fn the_step_is_order_independent() {
        // The Jacobi property, for pedestrians: the same crowd stepped from the same state
        // gives the same result whatever order the model happens to hold them in, which is
        // what the frozen positions guarantee. Spawning in reverse order is the test.
        let (w, cycle) = pavement_ring();
        let run = |ids: Vec<u32>| {
            let rng = RngRegistry::new(21);
            let mut m = SocialForce::default();
            {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                for id in ids {
                    m.spawn(
                        &mut ctx,
                        ActorId::new(id),
                        cycle.clone(),
                        f64::from(id) * 1.5,
                    )
                    .expect("spawned");
                }
            }
            let snapshot = ActorSnapshot::new(0, 50.0);
            let dt = Duration::from_millis(100);
            let mut last = Vec::new();
            for k in 0..50u64 {
                let mut ctx = MobilityCtx::new(k * 100 * NS_PER_MS, &w, &rng);
                last = m.step(&mut ctx, dt, &snapshot);
            }
            last
        };
        let forward = run((1..=8).collect());
        let reverse = run((1..=8).rev().collect());
        assert_eq!(forward, reverse);
    }

    #[test]
    fn the_card_validates() {
        let m = SocialForce::default();
        m.card().validate().expect("validates");
        assert_eq!(m.card().family, Family::Vru);
        for p in &m.card().parameters {
            if p.source.kind == SourceKind::TodoCalibrate {
                assert!(p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()));
            }
        }
    }
}
