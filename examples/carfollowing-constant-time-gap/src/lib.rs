//! **Worked example — the `CarFollowing` seam.** A constant-time-gap controller.
//!
//! The engine's own longitudinal model is the Intelligent Driver Model, which is a
//! *behavioural* model: it was fitted to how humans drive. This example is a *controller*:
//! the proportional-derivative law an adaptive-cruise-control system actually implements
//! over a constant-time-gap spacing policy. Putting the two behind one trait is the point
//! of the seam — a study of how ACC penetration changes a fundamental diagram needs both
//! in one run, and neither has to know the other exists.
//!
//! # What it computes
//!
//! ```text
//! spacing error   e = gap − (s0 + T·v)
//! following       a = k_p·e + k_d·(v_lead − v)
//! free road       a = k_v·(v0 − v)
//! ```
//!
//! then clamps the result to the driver's own comfortable acceleration and deceleration.
//! `T` and `s0` are the driver's — they arrive in [`DriverProfile`], because driver
//! heterogeneity is a per-vehicle draw — so the *model's* own parameters are the three
//! gains and nothing else.
//!
//! # Why the gains are `todo-calibrate`, and what that costs
//!
//! Nobody publishes gains for this: a real ACC's are a manufacturer's calibration. The
//! card therefore declares all three with `SourceKind::TodoCalibrate` and a plan, which
//! the registry *requires* (card rule R1) and which puts them on the generated
//! calibration-debt page until the plan is carried out. That is the honest state of this
//! model, and it is why its card says `unvalidated` rather than `unit-tested`: the tests
//! below check the algebra, and the algebra is not the claim in doubt.
//!
//! **A result from this model is provisional until the gains are fitted.** The plan is on
//! the card; the short version is: sweep the gains over the string-stability region and
//! pick the pair whose steady-state flow–density curve matches the fundamental-diagram
//! targets of `04-models.md` §2.9.
//!
//! # The rule this crate demonstrates that the others do not
//!
//! **`accel` takes no context, and that is a design statement, not an omission.** A
//! car-following model is required to be pure: same inputs, same output, on every
//! platform, with no random draw (driver heterogeneity is drawn once, per vehicle, and
//! arrives in the profile). So there is no `ctx` to pass, no stream to key, and the
//! determinism question does not arise. When a trait can be made pure, it is — that is
//! cheaper than any amount of testing.
//!
//! # What it is not
//!
//! * Not string-stable at arbitrary gains. Choosing them badly makes a platoon oscillate,
//!   which is a real property of a real controller and is why the calibration plan names
//!   the stability region.
//! * Not a model of how humans drive. Use [`v2xw_mobility::Idm`] for that.
//! * No reaction time, no driver imperfection, no lateral coupling. `ignores` says so.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use serde_json::json;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::weather::WeatherState;
use v2xw_mobility::traits::CarFollowing;
use v2xw_mobility::views::{DriverProfile, LaneView, LeaderView, VehicleView};
use v2xw_mobility::weather::{RoadContext, WeatherResponse, driving_effects};

/// The model's stable id.
pub const MODEL_ID: &str = "example/mobility/constant-time-gap";

/// The model's own version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The three gains, and the weather table the model reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CtgParams {
    /// Proportional gain on the spacing error, 1/s². **Uncalibrated.**
    pub k_position: f64,
    /// Derivative gain on the relative speed, 1/s. **Uncalibrated.**
    pub k_speed: f64,
    /// Gain of the free-road speed controller, 1/s. **Uncalibrated.**
    pub k_free: f64,
    /// Which cited weather table to apply (`04-models.md` §2.6).
    pub weather_response: WeatherResponse,
    /// Which rows of the FHWA table apply.
    pub road_context: RoadContext,
}

impl Default for CtgParams {
    /// Gains of 0.2, 0.6 and 0.4, which satisfy Rajamani's sign condition
    /// (`k_p, k_d > 0`) and are otherwise a starting point, not a result. See the card.
    ///
    /// The weather default is the FHWA table on arterial rows, which is what the engine's
    /// urban scenarios use.
    fn default() -> Self {
        Self {
            k_position: 0.2,
            k_speed: 0.6,
            k_free: 0.4,
            weather_response: WeatherResponse::Fhwa,
            road_context: RoadContext::Arterial,
        }
    }
}

/// The model. Holds its card and its gains; no per-vehicle state, because there is none to
/// hold — every input arrives in the views.
#[derive(Debug, Clone)]
pub struct ConstantTimeGap {
    card: ModelCard,
    params: CtgParams,
}

impl ConstantTimeGap {
    /// A controller with `params`.
    #[must_use]
    pub fn new(params: CtgParams) -> Self {
        Self {
            card: card(&params),
            params,
        }
    }

    /// A controller with the starting-point gains.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(CtgParams::default())
    }

    /// The gains this instance computes with.
    #[must_use]
    pub const fn params(&self) -> &CtgParams {
        &self.params
    }

    /// The control law, without the views.
    ///
    /// Public and separately testable on purpose: this is the whole model, and a reader
    /// should be able to check it against the equations in the crate documentation without
    /// constructing a `VehicleView`.
    ///
    /// `gap_m` is the **net** gap — leader rear to follower front, the leader's length
    /// already subtracted — which is the one meaning `v2xw_mobility::views` gives it
    /// everywhere. A model that read it as a centre-to-centre distance would be wrong by
    /// one vehicle length, in the direction that makes a crash look safe.
    #[must_use]
    pub fn control(
        &self,
        speed_mps: f64,
        desired_speed_mps: f64,
        gap_m: Option<f64>,
        leader_speed_mps: f64,
        driver: &DriverProfile,
    ) -> f64 {
        let raw = match gap_m {
            // Free road: hold the desired speed. No gap, no spacing term.
            None => self.params.k_free * (desired_speed_mps - speed_mps),
            Some(gap) => {
                // The constant-time-gap spacing policy: the gap a driver wants grows with
                // its own speed, which is what makes the policy string-stabilisable at
                // all (a constant-*distance* policy is not).
                let wanted = driver.min_gap_m + driver.time_headway_s * speed_mps;
                let error = gap - wanted;
                let closing = leader_speed_mps - speed_mps;
                let following = self.params.k_position * error + self.params.k_speed * closing;
                // The free-road term still applies: without it the controller would happily
                // accelerate past the speed limit behind a speeding leader.
                let free = self.params.k_free * (desired_speed_mps - speed_mps);
                following.min(free)
            }
        };
        // Clamped to what *this driver's* vehicle will do. `comfort_decel_mps2` is a
        // positive magnitude, so the floor is its negation.
        raw.clamp(-driver.comfort_decel_mps2, driver.max_accel_mps2)
    }
}

impl Model for ConstantTimeGap {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl CarFollowing for ConstantTimeGap {
    fn accel(
        &self,
        ego: &VehicleView,
        leader: Option<&LeaderView>,
        lane: &LaneView,
        w: &WeatherState,
    ) -> f64 {
        // The one place weather is applied, and it is applied the way the engine's own
        // car-following model applies it: the legal limit and the driver's wish are
        // reconciled first, then the weather slows the result. Scaling only the driver's
        // wish would leave an urban run where the limit binds completely unaffected by
        // snow.
        let effects = driving_effects(self.params.weather_response, w, self.params.road_context);
        let desired = effects
            .apply_to_desired_speed(ego.driver.desired_speed_mps.min(lane.speed_limit_mps));
        let driver = DriverProfile {
            desired_speed_mps: desired,
            max_accel_mps2: ego.driver.max_accel_mps2,
            comfort_decel_mps2: effects.cap_decel(ego.driver.comfort_decel_mps2),
            time_headway_s: effects.apply_to_headway(ego.driver.time_headway_s),
            min_gap_m: ego.driver.min_gap_m,
        };
        // A virtual obstacle — a red signal, a junction to yield at, a bend — arrives as a
        // leader standing still a known gap ahead, so one equation produces every
        // deceleration the vehicle applies. Nothing here has to know which it is.
        match leader {
            None => self.control(ego.speed_mps, desired, None, 0.0, &driver),
            Some(l) => self.control(
                ego.speed_mps,
                desired,
                Some(l.gap_m),
                l.speed_mps,
                &driver,
            ),
        }
    }

    // `profile` is deliberately **not** overridden. The default is the Kesting 2010 set,
    // which is §2.1's medium-tier default, and the trait documents that borrowing it is an
    // explicit statement rather than an accident. A model with its own per-class
    // calibration overrides it; this one has none, and saying so is better than inventing
    // one.
}

/// Builds the card.
#[must_use]
pub fn card(params: &CtgParams) -> ModelCard {
    let rajamani = Source {
        kind: SourceKind::Paper,
        reference: "R. Rajamani, Vehicle Dynamics and Control, 2nd ed., Springer 2012, \
                    chapter 5 (adaptive cruise control: the constant-time-gap spacing \
                    policy and its string-stability condition)"
            .to_string(),
        accessed: None,
        note: Some(
            "The source of the spacing policy and of the sign condition on the gains. It \
             is NOT the source of the gain values: those are a manufacturer calibration \
             in any real system, and the three defaults here are a starting point."
                .to_string(),
        ),
    };
    // One plan, stated once, referenced by all three gains. Naming the exact field that
    // has to change is what makes it a plan rather than an intention.
    let plan = "Sweep the gain triple over the region Rajamani's string-stability \
                condition admits and pick the point whose steady-state flow-density curve \
                matches the fundamental-diagram targets of 04-models.md §2.9 within their \
                stated tolerance. The harness is `v2xw_mobility::fd`, which today takes an \
                `IdmPreset` and drives `Idm` directly (`FdParams::preset`); calibrating a \
                non-IDM model against it requires that field to become a \
                `Box<dyn CarFollowing>` first. Until that is done these three numbers are \
                an implementer's guess and any result sensitive to them is provisional.";

    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Mobility,
        MODEL_VERSION,
        "Worked example. A proportional-derivative longitudinal controller over a \
         constant-time-gap spacing policy: what an adaptive-cruise-control system \
         implements, as opposed to the Intelligent Driver Model's account of how humans \
         drive. Pure, context-free and draw-free by construction.",
    );
    card.tier = vec![Tier::Medium];

    card.equations = vec![
        Equation {
            name: "spacing policy".to_string(),
            latex_or_text: "s_desired(v) = s0 + T·v".to_string(),
            notes: Some(
                "s0 and T are the driver's, from DriverProfile, not the model's: \
                 heterogeneity is a per-vehicle draw."
                    .to_string(),
            ),
        },
        Equation {
            name: "following law".to_string(),
            latex_or_text: "a = k_p·(gap − s_desired(v)) + k_d·(v_lead − v)".to_string(),
            notes: Some(
                "The result is then capped by the free-road term, so the controller does \
                 not chase a speeding leader past the limit."
                    .to_string(),
            ),
        },
        Equation {
            name: "free-road law".to_string(),
            latex_or_text: "a = k_v·(v0 − v)".to_string(),
            notes: Some(
                "v0 is min(driver's desired speed, lane limit), then scaled by the \
                 weather table."
                    .to_string(),
            ),
        },
        Equation {
            name: "clamp".to_string(),
            latex_or_text: "a := clamp(a, −b_comfort, a_max)".to_string(),
            notes: Some(
                "Both bounds are the driver's, with the deceleration bound additionally \
                 capped by what the weather says the surface can deliver."
                    .to_string(),
            ),
        },
    ];

    card.parameters = vec![
        Parameter {
            range: Some(vec![json!(0.01), json!(2.0)]),
            calibration: Some(plan.to_string()),
            ..Parameter::new(
                "k_position",
                "1/s^2",
                json!(params.k_position),
                Source::todo_calibrate(
                    "proportional gain on the spacing error; no published value exists \
                     for a production ACC",
                ),
            )
        },
        Parameter {
            range: Some(vec![json!(0.01), json!(3.0)]),
            calibration: Some(plan.to_string()),
            ..Parameter::new(
                "k_speed",
                "1/s",
                json!(params.k_speed),
                Source::todo_calibrate("derivative gain on the relative speed"),
            )
        },
        Parameter {
            range: Some(vec![json!(0.01), json!(3.0)]),
            calibration: Some(plan.to_string()),
            ..Parameter::new(
                "k_free",
                "1/s",
                json!(params.k_free),
                Source::todo_calibrate("gain of the free-road speed controller"),
            )
        },
    ];

    // The weather table's own parameters, which the model reads and must therefore
    // declare (invariant I-C3). They come from the crate that owns the table, so the
    // citation cannot drift from the numbers.
    card.parameters
        .extend(v2xw_mobility::weather::fhwa_parameters());

    card.assumptions = vec![
        "The leader's speed is known exactly and instantaneously: a real ACC estimates it \
         from a radar and this model does not."
            .to_string(),
        "gap_m is the net gap, leader rear to follower front.".to_string(),
        "One controller drives every equipped vehicle; there is no per-vehicle gain \
         variation."
            .to_string(),
    ];

    card.limitations = vec![
        "All three gains are uncalibrated. See each parameter's calibration plan; a result \
         sensitive to them is provisional."
            .to_string(),
        "String stability is not enforced anywhere in the code. A badly chosen gain triple \
         makes a platoon oscillate, and nothing here will tell you that it did — read the \
         speed time series."
            .to_string(),
        "The free-road cap makes the following law non-smooth where the two terms cross, \
         so the acceleration is continuous but not differentiable there."
            .to_string(),
    ];

    card.ignores = vec![
        "Reaction time and actuator lag: the response is instantaneous.".to_string(),
        "Driver imperfection: there is no acceleration noise, which is also why the model \
         draws no random numbers."
            .to_string(),
        "Lateral coupling and cut-ins: a vehicle changing into the gap is seen only once \
         it is the leader."
            .to_string(),
        "Anticipation of more than one leader, which the enhanced IDM models.".to_string(),
    ];

    card.sources = vec![rajamani];

    // `unvalidated`, not `unit-tested`, and the difference matters. The tests below check
    // the arithmetic against the equations above; nothing has checked the *gains*, which
    // are the part in doubt, so the honest status is the lower one. The scenario validator
    // warns when an unvalidated model is used at the `high` tier, which is exactly the
    // warning this model should produce.
    card.validation = Validation {
        status: ValidationStatus::Unvalidated,
        references: Vec::new(),
        tests: vec![
            "at_the_desired_gap_and_speed_the_acceleration_is_zero".to_string(),
            "a_closing_gap_brakes_and_an_opening_gap_does_not".to_string(),
            "the_clamp_is_the_drivers_own_limits".to_string(),
            "bad_weather_lowers_the_free_road_acceleration".to_string(),
        ],
    };

    // Nothing is drawn, so nothing is declared. The conformance kit checks this against
    // what the model actually did.
    card.determinism = Determinism::default();

    card
}

/// Registers the model.
///
/// # Errors
/// Whatever the registry refused, by name.
pub fn register(
    registry: &mut v2xw_core::registry::Registry,
) -> Result<v2xw_core::registry::ModelRef, v2xw_core::registry::RegistryError> {
    let model: v2xw_core::model::ModelHandle = std::sync::Arc::new(ConstantTimeGap::with_defaults());
    registry.register_model(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::geom::Dims;
    use v2xw_core::ids::{ActorId, LaneId};
    use v2xw_core::weather::{SurfaceCondition, WeatherKind, WeatherState};
    use v2xw_mobility::classes::VehicleClass;
    use v2xw_world::{ClassMask, LaneKind};

    fn driver() -> DriverProfile {
        // The Kesting 2010 set for a passenger car, which is what the trait's default
        // `profile` would hand this model anyway.
        DriverProfile {
            desired_speed_mps: 33.33,
            max_accel_mps2: 1.0,
            comfort_decel_mps2: 1.5,
            time_headway_s: 1.5,
            min_gap_m: 2.0,
        }
    }

    fn lane(limit_mps: f64) -> LaneView {
        LaneView {
            id: LaneId::new(0),
            kind: LaneKind::Driving,
            speed_limit_mps: limit_mps,
            width_m: 3.5,
            length_m: 500.0,
            allowed: ClassMask::MOTOR_TRAFFIC,
        }
    }

    fn ego(speed_mps: f64) -> VehicleView {
        VehicleView {
            actor: ActorId::new(0),
            class: VehicleClass::Passenger,
            lane: LaneId::new(0),
            lane_index: 0,
            s_m: 100.0,
            lateral_m: 0.0,
            speed_mps,
            accel_mps2: 0.0,
            heading_rad: 0.0,
            dims: Dims::new(5.0, 1.8, 1.5),
            driver: driver(),
        }
    }

    #[test]
    fn the_card_validates() {
        card(&CtgParams::default())
            .validate()
            .expect("the card must validate");
    }

    /// The card would be refused without the calibration plans, and this test is how a
    /// reader learns that rule R1 is enforced rather than advisory.
    ///
    /// **Shown to fail:** removing `calibration` from any of the three gains makes
    /// `validate` return `MissingCalibration` and this test go red.
    #[test]
    fn every_uncited_default_carries_a_plan() {
        let c = card(&CtgParams::default());
        let mut uncited = 0;
        for parameter in &c.parameters {
            if parameter.source.kind == SourceKind::TodoCalibrate {
                uncited += 1;
                assert!(
                    parameter
                        .calibration
                        .as_deref()
                        .is_some_and(|p| !p.trim().is_empty()),
                    "{} has no calibration plan",
                    parameter.name
                );
            }
        }
        assert!(uncited >= 3, "expected the three gains to be uncited");
    }

    #[test]
    fn at_the_desired_gap_and_speed_the_acceleration_is_zero() {
        let m = ConstantTimeGap::with_defaults();
        let d = driver();
        let v = 20.0;
        // Exactly the gap the policy wants, and the leader at the same speed: both terms
        // vanish, so the only thing left is the free-road term, which is positive because
        // 20 m/s is below the desired 33.33 m/s — and the `min` picks the smaller, which
        // is the following term's zero.
        let gap = d.min_gap_m + d.time_headway_s * v;
        let a = m.control(v, d.desired_speed_mps, Some(gap), v, &d);
        assert!(a.abs() < 1e-12, "expected 0, got {a}");
    }

    #[test]
    fn a_closing_gap_brakes_and_an_opening_gap_does_not() {
        let m = ConstantTimeGap::with_defaults();
        let d = driver();
        let v = 20.0;
        let wanted = d.min_gap_m + d.time_headway_s * v;
        let braking = m.control(v, d.desired_speed_mps, Some(wanted - 10.0), v, &d);
        let holding = m.control(v, d.desired_speed_mps, Some(wanted), v, &d);
        let opening = m.control(v, d.desired_speed_mps, Some(wanted + 10.0), v, &d);
        assert!(braking < 0.0, "a gap 10 m too small must brake, got {braking}");
        assert!(holding.abs() < 1e-12);
        assert!(
            opening > 0.0,
            "a gap 10 m too large must accelerate, got {opening}"
        );
        // Monotone in the gap, which is the property that makes the controller a
        // controller rather than a table.
        assert!(braking < holding && holding < opening);
    }

    #[test]
    fn the_clamp_is_the_drivers_own_limits() {
        let m = ConstantTimeGap::with_defaults();
        let d = driver();
        // A gap 500 m too small asks for an enormous deceleration; the driver's comfort
        // limit is what comes out.
        let hard = m.control(20.0, d.desired_speed_mps, Some(0.0), 0.0, &d);
        assert!((hard + d.comfort_decel_mps2).abs() < 1e-12, "got {hard}");
        // …and a vast free road asks for more than the vehicle will give.
        let fast = m.control(0.0, 100.0, None, 0.0, &d);
        assert!((fast - d.max_accel_mps2).abs() < 1e-12, "got {fast}");
    }

    #[test]
    fn the_speed_limit_binds_even_behind_a_speeding_leader() {
        let m = ConstantTimeGap::with_defaults();
        // A huge gap and a leader doing 40 m/s in a 15 m/s lane: the ego must not chase it.
        let a = m.accel(
            &ego(15.0),
            Some(&LeaderView::virtual_obstacle(300.0, 40.0)),
            &lane(15.0),
            &WeatherState::CLEAR,
        );
        assert!(
            a <= 1e-12,
            "at the limit with an open road the acceleration must not be positive, got {a}"
        );
    }

    #[test]
    fn bad_weather_lowers_the_free_road_acceleration() {
        // 23 m/s in a 25 m/s lane. In clear weather that is just under the limit, so the
        // controller accelerates gently; the FHWA arterial snow row cuts the desired speed
        // by the 30-40 % band's midpoint, to 16.25 m/s, which puts the same vehicle *above*
        // its desired speed and makes the controller brake.
        //
        // The speeds are chosen so that neither answer is at the clamp: a test in which
        // both sides saturate at the driver's 1.0 m/s^2 would pass whether or not the
        // weather table was ever consulted, which is the "check that cannot fail" this
        // repository keeps finding. Raising the ego speed to 5 m/s reproduces exactly that
        // failure mode, and is how this test was checked.
        let m = ConstantTimeGap::with_defaults();
        let clear = m.accel(&ego(23.0), None, &lane(25.0), &WeatherState::CLEAR);
        let snow = m.accel(
            &ego(23.0),
            None,
            &lane(25.0),
            &WeatherState {
                kind: WeatherKind::Snow,
                intensity: 1.0,
                visibility_m: 200.0,
                surface: SurfaceCondition::Snow,
            },
        );
        assert!(
            clear > 0.0 && clear < 1.0,
            "clear weather must accelerate, unclamped: got {clear}"
        );
        assert!(snow < 0.0, "heavy snow must brake at this speed: got {snow}");
        assert!(snow < clear);
    }

    #[test]
    fn a_virtual_obstacle_stops_the_vehicle() {
        // A red signal is a leader standing still. This is the assertion that says one
        // equation produces every deceleration.
        let m = ConstantTimeGap::with_defaults();
        let a = m.accel(
            &ego(15.0),
            Some(&LeaderView::virtual_obstacle(5.0, 0.0)),
            &lane(15.0),
            &WeatherState::CLEAR,
        );
        assert!(a < 0.0, "a stop line 5 m ahead at 15 m/s must brake, got {a}");
    }

    #[test]
    fn the_law_is_pure() {
        // Same inputs, same answer, however many times it is asked. Trivially true here,
        // and worth asserting because the moment a model acquires state this is the test
        // that notices.
        let m = ConstantTimeGap::with_defaults();
        let once = m.accel(
            &ego(12.0),
            Some(&LeaderView::virtual_obstacle(40.0, 10.0)),
            &lane(20.0),
            &WeatherState::CLEAR,
        );
        for _ in 0..8 {
            let again = m.accel(
                &ego(12.0),
                Some(&LeaderView::virtual_obstacle(40.0, 10.0)),
                &lane(20.0),
                &WeatherState::CLEAR,
            );
            assert_eq!(once, again);
        }
    }
}
