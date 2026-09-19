//! Weather state and the driving effects derived from it.
//!
//! Two structs, both of them arguments to model methods on either side of a crate boundary
//! (03-interfaces.md §3 and §4), which is why they live here and not in whichever crate
//! happens to implement a weather model first:
//!
//! * `WeatherModel::state_at(t, p) -> WeatherState` is produced in one crate and consumed
//!   in four. `CarFollowing::accel`, `LaneChange::decide` and `IntersectionControl::may_enter`
//!   in `v2xw-mobility` all take `&WeatherState`; so does `Propagation::loss_db` in
//!   `v2xw-radio`, which needs the rain rate for attenuation at 5.9 GHz, and so does the
//!   renderer, through the recording.
//! * `WeatherModel::driving_effects(&WeatherState) -> DrivingEffects` is the *interpretation*
//!   of that state as driver behaviour, and it is deliberately a separate call: the physical
//!   state is one model's output, and how much drivers slow down for it is a different,
//!   separately cited model (04-models.md §2.6).
//!
//! # No calibrated numbers live here
//!
//! This module defines the *shape* of the state and the effects, not their values. There is
//! no "rain reduces desired speed by 8%" anywhere in it: that number belongs to a weather
//! model's card, with a source, where invariant I-C3 can see it. The constructors here are
//! the two neutral elements — [`WeatherState::CLEAR`] and [`DrivingEffects::UNAFFECTED`] —
//! which assert nothing about the world.

use serde::{Deserialize, Serialize};

use crate::math;

/// What the weather *is*.
///
/// Coarse on purpose: these are the classes that change driving behaviour and radio
/// propagation differently from one another. Intensity within a class is
/// [`WeatherState::intensity`], so "light rain" and "downpour" are one variant with two
/// intensities rather than two variants.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum WeatherKind {
    /// No precipitation and no visibility restriction.
    #[default]
    Clear,
    /// Liquid precipitation. The one that matters for 5.9 GHz attenuation, and the
    /// commonest cause of a wet surface.
    Rain,
    /// Frozen precipitation: reduces visibility far more than the equivalent water content
    /// of rain, and changes the surface.
    Snow,
    /// Sleet or freezing rain — precipitation that falls wet and freezes on the surface.
    /// Separated from both neighbours because its surface effect is the worst of the two.
    Sleet,
    /// Suspended water droplets with no precipitation: visibility collapses, the road may
    /// stay dry, and the radio is barely affected. The case that separates a
    /// visibility-driven effect from a precipitation-driven one.
    Fog,
    /// Strong wind with no precipitation. No visibility or surface effect; it is here
    /// because a VRU and a high-sided vehicle model may react to it.
    Wind,
}

impl WeatherKind {
    /// Every kind. For reports and exhaustiveness tests.
    pub const ALL: [WeatherKind; 6] = [
        WeatherKind::Clear,
        WeatherKind::Rain,
        WeatherKind::Snow,
        WeatherKind::Sleet,
        WeatherKind::Fog,
        WeatherKind::Wind,
    ];

    /// True if this kind deposits water or ice on the road surface.
    pub const fn is_precipitation(self) -> bool {
        matches!(
            self,
            WeatherKind::Rain | WeatherKind::Snow | WeatherKind::Sleet
        )
    }

    /// The kebab-case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            WeatherKind::Clear => "clear",
            WeatherKind::Rain => "rain",
            WeatherKind::Snow => "snow",
            WeatherKind::Sleet => "sleet",
            WeatherKind::Fog => "fog",
            WeatherKind::Wind => "wind",
        }
    }
}

impl core::fmt::Display for WeatherKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The state of the road surface, which is what a friction-dependent model actually reads.
///
/// Separate from [`WeatherKind`] because the two decouple in both directions: a road stays
/// wet after the rain stops, and it can be icy under a clear sky. A car-following model
/// that keyed its braking limit off the precipitation class would get both of those wrong.
///
/// [`Ord`] runs from best to worst grip.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SurfaceCondition {
    /// Dry asphalt.
    #[default]
    Dry,
    /// Wet, with no standing water.
    Wet,
    /// Standing water deep enough for aquaplaning to be a consideration.
    Flooded,
    /// Loose snow on the surface.
    Snow,
    /// Ice, including black ice under a clear sky.
    Ice,
}

impl SurfaceCondition {
    /// Every condition, best grip first.
    pub const ALL: [SurfaceCondition; 5] = [
        SurfaceCondition::Dry,
        SurfaceCondition::Wet,
        SurfaceCondition::Flooded,
        SurfaceCondition::Snow,
        SurfaceCondition::Ice,
    ];

    /// The kebab-case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            SurfaceCondition::Dry => "dry",
            SurfaceCondition::Wet => "wet",
            SurfaceCondition::Flooded => "flooded",
            SurfaceCondition::Snow => "snow",
            SurfaceCondition::Ice => "ice",
        }
    }
}

impl core::fmt::Display for SurfaceCondition {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The weather at one place and one instant (03-interfaces.md §3).
///
/// Produced by `WeatherModel::state_at`, consumed by mobility, propagation and the
/// renderer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WeatherState {
    /// What the weather is.
    pub kind: WeatherKind,
    /// How hard it is doing it, on `[0, 1]`: `0` is the onset, `1` is the heaviest this
    /// kind is modelled at.
    ///
    /// Dimensionless and normalised on purpose. A millimetre-per-hour rain rate is the
    /// input a radio attenuation model wants and a *fraction of maximum* is what a
    /// behavioural model wants, and the two scales differ per kind (1 mm/h of rain and
    /// 1 mm/h water equivalent of snow are not comparable events). So the scale is fixed
    /// here and each model's card declares what its own `1.0` corresponds to in physical
    /// units, where the number can be cited.
    pub intensity: f64,
    /// Meteorological visibility, metres: the distance at which a large dark object is
    /// just discernible.
    ///
    /// [`f64::INFINITY`] means "unrestricted", which is what [`WeatherState::CLEAR`]
    /// carries. A model that prefers the 10 km convention sets 10,000 explicitly and says
    /// so on its card. JSON cannot carry an infinity, so the field is encoded through
    /// [`crate::serde_sentinel::f64_inf`]: the sentinel is written as `null` — which is what
    /// `serde_json` wrote for it anyway — and `null` is read back as the sentinel, so
    /// `CLEAR` survives its own round trip. The binary wire protocol carries it as it is.
    #[serde(with = "crate::serde_sentinel::f64_inf")]
    pub visibility_m: f64,
    /// What the road surface is like, which need not follow from `kind`.
    pub surface: SurfaceCondition,
}

impl WeatherState {
    /// The declared quantum for [`WeatherState::visibility_m`]: 1 mm, the metre grid of
    /// build decision D9.
    pub const Q_VISIBILITY_M: f64 = 1e-3;

    /// The declared quantum for [`WeatherState::intensity`]: 1e-4, the grid D9 gives
    /// dimensionless ratios.
    pub const Q_INTENSITY: f64 = 1e-4;

    /// Clear, dry, unrestricted: the neutral element, and the state a scenario that says
    /// nothing about weather runs in.
    pub const CLEAR: WeatherState = WeatherState {
        kind: WeatherKind::Clear,
        intensity: 0.0,
        visibility_m: f64::INFINITY,
        surface: SurfaceCondition::Dry,
    };

    /// A state, with `intensity` clamped to `[0, 1]`.
    ///
    /// Clamping rather than rejecting: a weather model that interpolates a timeline can
    /// overshoot by a rounding error at a knot, and failing a run for `1.0000000000000002`
    /// would be absurd. A grossly out-of-range value is a model bug, which
    /// [`WeatherState::is_well_formed`] is there to catch in a conformance test.
    pub fn new(
        kind: WeatherKind,
        intensity: f64,
        visibility_m: f64,
        surface: SurfaceCondition,
    ) -> Self {
        Self {
            kind,
            intensity: if intensity.is_nan() {
                0.0
            } else {
                intensity.clamp(0.0, 1.0)
            },
            visibility_m,
            surface,
        }
    }

    /// True if visibility is restricted at all.
    pub fn is_visibility_reduced(&self) -> bool {
        self.visibility_m.is_finite()
    }

    /// True if the fields are within their documented domains: intensity on `[0, 1]`, and
    /// visibility non-negative (infinite allowed) rather than negative or `NaN`.
    pub fn is_well_formed(&self) -> bool {
        (0.0..=1.0).contains(&self.intensity)
            && !self.visibility_m.is_nan()
            && self.visibility_m >= 0.0
    }

    /// This state with every float on its declared grid (build decision D9), for recording
    /// and for any threshold comparison that is made across engines (D10).
    pub fn quantized(&self) -> Self {
        Self {
            kind: self.kind,
            intensity: math::quantize_to(self.intensity, Self::Q_INTENSITY),
            visibility_m: math::quantize_to(self.visibility_m, Self::Q_VISIBILITY_M),
            surface: self.surface,
        }
    }
}

impl Default for WeatherState {
    /// [`WeatherState::CLEAR`].
    fn default() -> Self {
        Self::CLEAR
    }
}

/// How the weather changes driving, as a bundle of multipliers and caps
/// (03-interfaces.md §3, 04-models.md §2.6).
///
/// Returned by `WeatherModel::driving_effects` and applied by the car-following,
/// lane-change and intersection models. Multipliers rather than absolute values, so that
/// one weather model composes with any driver model: the driver model owns the desired
/// speed and the headway, the weather model owns the factor, and neither has to know the
/// other's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DrivingEffects {
    /// Multiplier on each driver's desired speed, `1.0` for no effect.
    pub desired_speed_factor: f64,
    /// Multiplier on the desired time headway, `1.0` for no effect. Greater than one in bad
    /// weather: drivers leave more room.
    pub headway_factor: f64,
    /// Upper bound on the deceleration a vehicle may use, m/s².
    ///
    /// A *cap*, not a factor, because it is a limit of the tyre-road contact rather than a
    /// preference: it is what the surface can deliver. [`f64::INFINITY`] means "not capped
    /// by the weather" and leaves the vehicle's own limit in force; it is encoded through
    /// [`crate::serde_sentinel::f64_inf`], so [`DrivingEffects::UNAFFECTED`] round-trips
    /// through JSON instead of serialising to a `null` that cannot be read back.
    #[serde(with = "crate::serde_sentinel::f64_inf")]
    pub max_decel_mps2: f64,
    /// The visibility the driver model should use, metres — normally
    /// [`WeatherState::visibility_m`], but a weather model may report a shorter effective
    /// value (spray behind a lorry, glare) and this is the number the models read.
    ///
    /// [`f64::INFINITY`] means "unrestricted", encoded as for
    /// [`DrivingEffects::max_decel_mps2`].
    #[serde(with = "crate::serde_sentinel::f64_inf")]
    pub visibility_m: f64,
}

impl DrivingEffects {
    /// The declared quantum for the two factors: 1e-4, the grid D9 gives ratios.
    pub const Q_FACTOR: f64 = 1e-4;

    /// The declared quantum for the metre and m/s² fields: 1 mm, 1 mm/s².
    pub const Q_M: f64 = 1e-3;

    /// No effect at all: both factors `1.0`, no deceleration cap, unrestricted visibility.
    ///
    /// The neutral element, and what a weather model must return for
    /// [`WeatherState::CLEAR`] if it is to leave a clear-weather run identical to a run
    /// with no weather model at all.
    pub const UNAFFECTED: DrivingEffects = DrivingEffects {
        desired_speed_factor: 1.0,
        headway_factor: 1.0,
        max_decel_mps2: f64::INFINITY,
        visibility_m: f64::INFINITY,
    };

    /// The desired speed `v_mps` as the weather leaves it.
    pub fn apply_to_desired_speed(&self, v_mps: f64) -> f64 {
        v_mps * self.desired_speed_factor
    }

    /// The desired time headway `t_s` as the weather leaves it.
    pub fn apply_to_headway(&self, t_s: f64) -> f64 {
        t_s * self.headway_factor
    }

    /// The deceleration `a_mps2` capped by the surface. Never increases it.
    pub fn cap_decel(&self, a_mps2: f64) -> f64 {
        a_mps2.min(self.max_decel_mps2)
    }

    /// True if the effects are usable: both factors finite and strictly positive, the
    /// deceleration cap positive (possibly infinite), visibility non-negative.
    ///
    /// A zero or negative speed factor would stop or reverse every vehicle in the scenario,
    /// and a zero deceleration cap would make braking impossible — both are model bugs that
    /// a conformance test should catch at the seam rather than three phases downstream in a
    /// pile-up.
    pub fn is_well_formed(&self) -> bool {
        self.desired_speed_factor.is_finite()
            && self.desired_speed_factor > 0.0
            && self.headway_factor.is_finite()
            && self.headway_factor > 0.0
            && self.max_decel_mps2 > 0.0
            && !self.max_decel_mps2.is_nan()
            && !self.visibility_m.is_nan()
            && self.visibility_m >= 0.0
    }

    /// These effects with every float on its declared grid (build decision D9).
    pub fn quantized(&self) -> Self {
        Self {
            desired_speed_factor: math::quantize_to(self.desired_speed_factor, Self::Q_FACTOR),
            headway_factor: math::quantize_to(self.headway_factor, Self::Q_FACTOR),
            max_decel_mps2: math::quantize_to(self.max_decel_mps2, Self::Q_M),
            visibility_m: math::quantize_to(self.visibility_m, Self::Q_M),
        }
    }
}

impl Default for DrivingEffects {
    /// [`DrivingEffects::UNAFFECTED`].
    fn default() -> Self {
        Self::UNAFFECTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neutral_states_really_are_neutral() {
        let w = WeatherState::CLEAR;
        assert_eq!(w, WeatherState::default());
        assert_eq!(w.kind, WeatherKind::Clear);
        assert_eq!(w.intensity, 0.0);
        assert_eq!(w.surface, SurfaceCondition::Dry);
        assert!(!w.is_visibility_reduced());
        assert!(w.is_well_formed());

        let e = DrivingEffects::UNAFFECTED;
        assert_eq!(e, DrivingEffects::default());
        assert_eq!(e.apply_to_desired_speed(13.9), 13.9);
        assert_eq!(e.apply_to_headway(1.4), 1.4);
        assert_eq!(e.cap_decel(9.0), 9.0, "an uncapped decel passes through");
        assert!(e.is_well_formed());
    }

    /// The effects are multipliers and a cap, and the cap only ever reduces.
    #[test]
    fn effects_scale_and_cap() {
        let e = DrivingEffects {
            desired_speed_factor: 0.8,
            headway_factor: 1.5,
            max_decel_mps2: 4.0,
            visibility_m: 120.0,
        };
        assert_eq!(e.apply_to_desired_speed(30.0), 24.0);
        assert!((e.apply_to_headway(1.2) - 1.8).abs() < 1e-15);
        assert_eq!(e.cap_decel(9.0), 4.0, "the surface limits the braking");
        assert_eq!(e.cap_decel(2.0), 2.0, "a gentler braking is untouched");
        assert!(e.is_well_formed());

        // The shapes that would wreck a run, caught at the seam.
        for bad in [
            DrivingEffects {
                desired_speed_factor: 0.0,
                ..e
            },
            DrivingEffects {
                desired_speed_factor: -1.0,
                ..e
            },
            DrivingEffects {
                desired_speed_factor: f64::NAN,
                ..e
            },
            DrivingEffects {
                headway_factor: 0.0,
                ..e
            },
            DrivingEffects {
                max_decel_mps2: 0.0,
                ..e
            },
            DrivingEffects {
                max_decel_mps2: f64::NAN,
                ..e
            },
            DrivingEffects {
                visibility_m: -1.0,
                ..e
            },
        ] {
            assert!(!bad.is_well_formed(), "{bad:?} should be rejected");
        }
        // An infinite cap is legal — it is the "no cap" spelling.
        assert!(
            DrivingEffects {
                max_decel_mps2: f64::INFINITY,
                ..e
            }
            .is_well_formed()
        );
    }

    /// Intensity is a normalised fraction, and the constructor keeps it one.
    #[test]
    fn intensity_is_clamped_to_the_unit_interval() {
        let w = WeatherState::new(
            WeatherKind::Rain,
            1.000_000_000_000_2,
            4_000.0,
            SurfaceCondition::Wet,
        );
        assert_eq!(w.intensity, 1.0);
        assert!(w.is_well_formed());
        assert_eq!(
            WeatherState::new(WeatherKind::Snow, -0.5, 500.0, SurfaceCondition::Snow).intensity,
            0.0
        );
        assert_eq!(
            WeatherState::new(WeatherKind::Fog, f64::NAN, 50.0, SurfaceCondition::Dry).intensity,
            0.0,
            "NaN is not a fraction"
        );
        // …but a field set directly is not clamped, which is what the predicate is for.
        let hand_built = WeatherState {
            intensity: 3.0,
            ..WeatherState::CLEAR
        };
        assert!(!hand_built.is_well_formed());
        assert!(
            !WeatherState {
                visibility_m: -1.0,
                ..WeatherState::CLEAR
            }
            .is_well_formed()
        );
        assert!(
            !WeatherState {
                visibility_m: f64::NAN,
                ..WeatherState::CLEAR
            }
            .is_well_formed()
        );
    }

    /// Fog is the case that proves the kind, the surface and the visibility are three
    /// independent axes rather than one.
    #[test]
    fn kind_surface_and_visibility_are_independent() {
        let fog = WeatherState::new(WeatherKind::Fog, 0.9, 40.0, SurfaceCondition::Dry);
        assert!(!fog.kind.is_precipitation());
        assert!(fog.is_visibility_reduced());
        assert_eq!(fog.surface, SurfaceCondition::Dry);

        // Black ice under a clear sky: no precipitation now, no visibility loss, no grip.
        let ice = WeatherState::new(
            WeatherKind::Clear,
            0.0,
            f64::INFINITY,
            SurfaceCondition::Ice,
        );
        assert!(!ice.is_visibility_reduced());
        assert_eq!(ice.surface, SurfaceCondition::Ice);
        assert!(ice.is_well_formed());

        assert!(WeatherKind::Rain.is_precipitation());
        assert!(WeatherKind::Sleet.is_precipitation());
        assert!(!WeatherKind::Wind.is_precipitation());
        assert!(
            SurfaceCondition::Dry < SurfaceCondition::Ice,
            "best grip first"
        );
    }

    /// Both types reach the recording, so both quantise, and the wire spelling is the
    /// documented kebab-case one.
    #[test]
    fn states_quantize_and_round_trip() {
        let w = WeatherState::new(
            WeatherKind::Rain,
            0.123_456_78,
            4_321.987_654,
            SurfaceCondition::Wet,
        )
        .quantized();
        assert_eq!(w.intensity, 0.1235);
        assert_eq!(w.visibility_m, 4_321.988);
        assert!(math::is_on_grid(w.intensity, WeatherState::Q_INTENSITY));
        assert!(math::is_on_grid(
            w.visibility_m,
            WeatherState::Q_VISIBILITY_M
        ));
        assert_eq!(w.quantized(), w);
        // The "unrestricted" sentinel survives quantisation.
        assert!(WeatherState::CLEAR.quantized().visibility_m.is_infinite());

        let e = DrivingEffects {
            desired_speed_factor: 0.812_345_6,
            headway_factor: 1.499_96,
            max_decel_mps2: 4.000_499_9,
            visibility_m: 120.000_6,
        }
        .quantized();
        assert_eq!(e.desired_speed_factor, 0.8123);
        assert_eq!(e.headway_factor, 1.5);
        assert_eq!(e.max_decel_mps2, 4.0);
        assert_eq!(e.visibility_m, 120.001);
        assert_eq!(e.quantized(), e);
        assert!(
            DrivingEffects::UNAFFECTED
                .quantized()
                .max_decel_mps2
                .is_infinite()
        );

        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains(r#""kind":"rain""#), "{json}");
        assert!(json.contains(r#""surface":"wet""#), "{json}");
        assert_eq!(serde_json::from_str::<WeatherState>(&json).unwrap(), w);
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<DrivingEffects>(&json).unwrap(), e);

        for k in WeatherKind::ALL {
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{k}\""));
        }
        for s in SurfaceCondition::ALL {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{s}\""));
        }
    }

    /// **The neutral elements must round-trip too.** They are the values that dominate a
    /// run — `CLEAR` is the weather of every scenario that says nothing about weather, and
    /// `UNAFFECTED` is what a weather model must return for it — and they are exactly the
    /// values the old encoding lost: `visibility_m: INFINITY` serialised to `null`, and the
    /// derived `Deserialize` for `f64` refuses `null`, so a run could record a clear-weather
    /// keyframe and then fail to replay its own recording. The tests above missed it because
    /// each round-trips a *finite* instance and probes the constant with `is_infinite()`.
    #[test]
    fn the_neutral_states_survive_a_json_round_trip() {
        let json = serde_json::to_string(&WeatherState::CLEAR).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"clear","intensity":0.0,"visibility_m":null,"surface":"dry"}"#
        );
        let back: WeatherState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, WeatherState::CLEAR);
        assert!(back.visibility_m.is_infinite() && back.visibility_m > 0.0);
        assert!(!back.is_visibility_reduced());

        let json = serde_json::to_string(&DrivingEffects::UNAFFECTED).unwrap();
        assert_eq!(
            json,
            r#"{"desired_speed_factor":1.0,"headway_factor":1.0,"max_decel_mps2":null,"visibility_m":null}"#
        );
        let back: DrivingEffects = serde_json::from_str(&json).unwrap();
        assert_eq!(back, DrivingEffects::UNAFFECTED);
        assert_eq!(back.cap_decel(9.0), 9.0, "still uncapped after the trip");
        assert!(back.is_well_formed());

        // YAML is the authoring format, and it round-trips too.
        let yaml = serde_yml::to_string(&WeatherState::CLEAR).unwrap();
        assert_eq!(
            serde_yml::from_str::<WeatherState>(&yaml).unwrap(),
            WeatherState::CLEAR
        );

        // A quantised neutral element is still the neutral element, and still round-trips:
        // the quantiser passes non-finite values through, and the codec carries them.
        let q = WeatherState::CLEAR.quantized();
        assert_eq!(
            serde_json::from_str::<WeatherState>(&serde_json::to_string(&q).unwrap()).unwrap(),
            WeatherState::CLEAR
        );
        let q = DrivingEffects::UNAFFECTED.quantized();
        assert_eq!(
            serde_json::from_str::<DrivingEffects>(&serde_json::to_string(&q).unwrap()).unwrap(),
            DrivingEffects::UNAFFECTED
        );

        // A finite reading is untouched by the codec in both directions.
        let fog = WeatherState::new(WeatherKind::Fog, 0.9, 40.0, SurfaceCondition::Dry);
        let json = serde_json::to_string(&fog).unwrap();
        assert!(json.contains(r#""visibility_m":40.0"#), "{json}");
        assert_eq!(serde_json::from_str::<WeatherState>(&json).unwrap(), fog);
    }
}
