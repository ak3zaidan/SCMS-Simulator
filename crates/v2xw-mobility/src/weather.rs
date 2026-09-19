//! How weather reaches a driver — 04-models.md §2.6.
//!
//! §2.6 gives two tables and assigns them to the `weather` family's `WeatherModel`
//! interface, whose `driving_effects` method returns a [`DrivingEffects`]. That family lives
//! in another crate; what mobility needs is the *table*, so the two cited tables are here as
//! pure functions and the mobility models select one with a [`WeatherResponse`] parameter.
//! When the weather crate lands it calls these same functions, and nothing in mobility
//! changes.
//!
//! **One place applies weather.** The car-following model scales the driver's desired speed,
//! its desired headway and its comfortable deceleration; nothing else in the crate touches
//! the weather. Applying it twice — once in the engine and once in the model — would square
//! the factor, which is exactly the kind of quiet error a single owner prevents.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{Parameter, Source, SourceKind};
use v2xw_core::weather::{DrivingEffects, WeatherKind, WeatherState};

/// Which cited table a model applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WeatherResponse {
    /// Ignore the weather: [`DrivingEffects::UNAFFECTED`]. What the abstract tier does,
    /// and what a run that has no weather model wants.
    Ignore,
    /// `weather/driving/legacy-multipliers` (§2.6, `code (legacy)` [`run.py` L137-139]):
    /// a desired-speed multiplier only.
    Legacy,
    /// `weather/driving/fhwa-table` (§2.6, FHWA Road Weather Management, R10 §B14): a
    /// speed factor and a headway factor from the band midpoints.
    #[default]
    Fhwa,
}

impl WeatherResponse {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            WeatherResponse::Ignore => "ignore",
            WeatherResponse::Legacy => "legacy",
            WeatherResponse::Fhwa => "fhwa",
        }
    }
}

/// Which FHWA rows apply: the freeway rows or the arterial rows (§2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoadContext {
    /// The freeway rows.
    #[default]
    Freeway,
    /// The arterial rows.
    Arterial,
}

/// The intensity at or above which a precipitation kind counts as "heavy" and takes the
/// heavy rows of the FHWA table.
///
/// **`TODO: calibrate`.** [`WeatherState::intensity`] is a normalised fraction of the
/// heaviest modelled event, and the FHWA table is written in words ("light", "heavy"), so
/// some threshold has to be chosen to join them. 0.5 is the midpoint and is recorded as a
/// design choice; the calibration plan is in [`fhwa_parameters`].
pub const HEAVY_INTENSITY_THRESHOLD: f64 = 0.5;

/// The legacy multipliers (§2.6, `code (legacy)`).
///
/// `clear` 1.0, `rain` 0.85, `fog` 0.75, `snow` 0.6, applied to the desired speed and to
/// nothing else — the legacy engine modelled no headway or capacity effect. The table has
/// four states; `sleet` takes the snow multiplier (the worse of its two neighbours) and
/// `wind` takes 1.0, and both mappings are design choices recorded here rather than values
/// read from anywhere.
pub fn legacy_driving_effects(w: &WeatherState) -> DrivingEffects {
    let factor = match w.kind {
        WeatherKind::Clear | WeatherKind::Wind => 1.0,
        WeatherKind::Rain => 0.85,
        WeatherKind::Fog => 0.75,
        WeatherKind::Snow | WeatherKind::Sleet => 0.6,
        // `WeatherKind` is `#[non_exhaustive]`: a kind added to core after this table was
        // written has no entry in the legacy table, and the neutral element is the only
        // answer that invents nothing.
        _ => 1.0,
    };
    DrivingEffects {
        desired_speed_factor: factor,
        headway_factor: 1.0,
        max_decel_mps2: f64::INFINITY,
        visibility_m: w.visibility_m,
    }
}

/// The FHWA table (§2.6, R10 §B14), applied at the band midpoints.
///
/// The bands are the cited values; using the midpoint of each band is the design choice
/// §2.6 records. `headway_factor = 1 / (1 - capacity reduction)`, which is the conversion
/// §2.6 prescribes: a capacity cut of `c` is reproduced by stretching every headway by
/// `1/(1 - c)`.
///
/// `max_decel_mps2` is **not** capped: the friction tables the cap needs are the
/// `TODO: calibrate` row of §2.6, and inventing a number here would put an uncited value
/// into every braking manoeuvre.
pub fn fhwa_driving_effects(w: &WeatherState, road: RoadContext) -> DrivingEffects {
    let heavy = w.intensity >= HEAVY_INTENSITY_THRESHOLD;
    // (speed reduction band, capacity reduction band), as fractions.
    let (speed, capacity) = match (road, w.kind, heavy) {
        (_, WeatherKind::Clear | WeatherKind::Wind, _) => ((0.0, 0.0), (0.0, 0.0)),
        // Freeway rows.
        (RoadContext::Freeway, WeatherKind::Rain, false) => ((0.03, 0.13), (0.04, 0.11)),
        (RoadContext::Freeway, WeatherKind::Rain, true) => ((0.03, 0.16), (0.10, 0.30)),
        (RoadContext::Freeway, WeatherKind::Snow | WeatherKind::Sleet, false) => {
            ((0.03, 0.13), (0.04, 0.11))
        }
        (RoadContext::Freeway, WeatherKind::Snow | WeatherKind::Sleet, true) => {
            ((0.05, 0.40), (0.12, 0.27))
        }
        (RoadContext::Freeway, WeatherKind::Fog, _) => ((0.10, 0.12), (0.12, 0.12)),
        // Arterial rows: the table is written by pavement state, so wet pavement covers
        // rain and fog-with-wet-surface, and snowy or slushy pavement covers snow and
        // sleet. The capacity column is the saturation-flow reduction (2-21 %) for the wet
        // row and the volume reduction (15-30 %) for the snow row, which are the two
        // numbers §2.6 gives for an arterial.
        (RoadContext::Arterial, WeatherKind::Rain | WeatherKind::Fog, _) => {
            ((0.10, 0.25), (0.02, 0.21))
        }
        (RoadContext::Arterial, WeatherKind::Snow | WeatherKind::Sleet, _) => {
            ((0.30, 0.40), (0.15, 0.30))
        }
        // `WeatherKind` is `#[non_exhaustive]`: a kind the FHWA table does not list takes
        // the neutral element rather than a guessed band.
        _ => ((0.0, 0.0), (0.0, 0.0)),
    };
    let mid = |band: (f64, f64)| 0.5 * (band.0 + band.1);
    let speed_cut = mid(speed);
    let capacity_cut = mid(capacity);
    DrivingEffects {
        desired_speed_factor: 1.0 - speed_cut,
        headway_factor: 1.0 / (1.0 - capacity_cut),
        max_decel_mps2: f64::INFINITY,
        visibility_m: w.visibility_m,
    }
}

/// The effects `response` produces for `w`.
pub fn driving_effects(
    response: WeatherResponse,
    w: &WeatherState,
    road: RoadContext,
) -> DrivingEffects {
    match response {
        WeatherResponse::Ignore => DrivingEffects::UNAFFECTED,
        WeatherResponse::Legacy => legacy_driving_effects(w),
        WeatherResponse::Fhwa => fhwa_driving_effects(w, road),
    }
}

/// The card parameters every model that reads the weather must declare (invariant I-C3).
pub fn fhwa_parameters() -> Vec<Parameter> {
    let fhwa = Source {
        kind: SourceKind::Standard,
        reference: "FHWA Road Weather Management, \"How Do Weather Events Impact Roads?\" \
                    (04-models.md §2.6, R10 §B14)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: Some(
            "the bands are cited; taking the midpoint of each band is the design choice \
             §2.6 records"
                .to_string(),
        ),
    };
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py L137-139 (`WEATHER_SPEED_MULT`)"
            .to_string(),
        accessed: Some("2026-09-18".to_string()),
        note: None,
    };
    vec![
        Parameter::new(
            "weather_response",
            "-",
            serde_json::json!("fhwa"),
            fhwa.clone(),
        ),
        Parameter::new(
            "weather.legacy_multipliers",
            "1",
            serde_json::json!({"clear": 1.0, "rain": 0.85, "fog": 0.75, "snow": 0.6}),
            legacy,
        ),
        Parameter::new(
            "weather.fhwa_freeway_speed_bands",
            "1",
            serde_json::json!({
                "light_rain_or_snow": [0.03, 0.13],
                "heavy_rain": [0.03, 0.16],
                "heavy_snow": [0.05, 0.40],
                "fog": [0.10, 0.12],
            }),
            fhwa.clone(),
        ),
        Parameter::new(
            "weather.fhwa_freeway_capacity_bands",
            "1",
            serde_json::json!({
                "light_rain_or_snow": [0.04, 0.11],
                "heavy_rain": [0.10, 0.30],
                "heavy_snow": [0.12, 0.27],
                "fog": [0.12, 0.12],
            }),
            fhwa.clone(),
        ),
        Parameter::new(
            "weather.fhwa_arterial_bands",
            "1",
            serde_json::json!({
                "wet_speed": [0.10, 0.25],
                "wet_saturation_flow": [0.02, 0.21],
                "snowy_speed": [0.30, 0.40],
                "snowy_volume": [0.15, 0.30],
            }),
            fhwa,
        ),
        Parameter {
            name: "weather.heavy_intensity_threshold".to_string(),
            unit: "1".to_string(),
            default: serde_json::json!(HEAVY_INTENSITY_THRESHOLD),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: Source::todo_calibrate(
                "the intensity at which the FHWA \"light\" rows give way to the \"heavy\" rows",
            ),
            calibration: Some(
                "The FHWA table is written in words and `WeatherState::intensity` is a \
                 normalised fraction, so the join needs a threshold. Plan: express the two \
                 FHWA classes in mm/h from a met-service precipitation-intensity scale, then \
                 set the threshold to the same fraction of the weather model's declared \
                 physical maximum for that kind (the card of every weather model states what \
                 its `intensity = 1.0` means in physical units)."
                    .to_string(),
            ),
        },
        Parameter {
            name: "weather.max_decel_mps2".to_string(),
            unit: "m/s²".to_string(),
            default: serde_json::json!(null),
            range: None,
            source: Source::todo_calibrate(
                "the deceleration a wet or icy surface can deliver (§2.6 `decel_cap`)",
            ),
            calibration: Some(
                "Plan: take friction-coefficient tables for wet, snowy and icy pavement from \
                 an AASHTO or FHWA source and cap the deceleration at µ·g. Until then the cap \
                 is absent (infinite) and the vehicle's own limit is in force, so no uncited \
                 number reaches a braking manoeuvre."
                    .to_string(),
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::weather::SurfaceCondition;

    fn state(kind: WeatherKind, intensity: f64) -> WeatherState {
        WeatherState {
            kind,
            intensity,
            visibility_m: 1000.0,
            surface: SurfaceCondition::Wet,
        }
    }

    #[test]
    fn legacy_multipliers_are_the_legacy_table() {
        assert_eq!(
            legacy_driving_effects(&WeatherState::CLEAR).desired_speed_factor,
            1.0
        );
        assert_eq!(
            legacy_driving_effects(&state(WeatherKind::Rain, 0.5)).desired_speed_factor,
            0.85
        );
        assert_eq!(
            legacy_driving_effects(&state(WeatherKind::Fog, 0.5)).desired_speed_factor,
            0.75
        );
        assert_eq!(
            legacy_driving_effects(&state(WeatherKind::Snow, 0.5)).desired_speed_factor,
            0.6
        );
        // The legacy table models no headway effect at all.
        assert_eq!(
            legacy_driving_effects(&state(WeatherKind::Snow, 1.0)).headway_factor,
            1.0
        );
    }

    #[test]
    fn fhwa_midpoints_match_the_bands() {
        // Heavy rain on a freeway: speed 3-16 % (midpoint 9.5 %), capacity 10-30 %
        // (midpoint 20 %).
        let e = fhwa_driving_effects(&state(WeatherKind::Rain, 0.9), RoadContext::Freeway);
        assert!((e.desired_speed_factor - (1.0 - 0.095)).abs() < 1e-12);
        assert!((e.headway_factor - 1.0 / (1.0 - 0.20)).abs() < 1e-12);
        // Fog: 11 % speed, 12 % capacity, both columns single-valued.
        let f = fhwa_driving_effects(&state(WeatherKind::Fog, 0.1), RoadContext::Freeway);
        assert!((f.desired_speed_factor - 0.89).abs() < 1e-12);
        assert!((f.headway_factor - 1.0 / 0.88).abs() < 1e-12);
        // Clear weather is the neutral element whichever table is chosen.
        for r in [
            WeatherResponse::Ignore,
            WeatherResponse::Legacy,
            WeatherResponse::Fhwa,
        ] {
            let n = driving_effects(r, &WeatherState::CLEAR, RoadContext::Freeway);
            assert_eq!(n.desired_speed_factor, 1.0);
            assert_eq!(n.headway_factor, 1.0);
        }
    }

    #[test]
    fn heavy_snow_is_worse_than_light_snow() {
        let light = fhwa_driving_effects(&state(WeatherKind::Snow, 0.2), RoadContext::Freeway);
        let heavy = fhwa_driving_effects(&state(WeatherKind::Snow, 0.8), RoadContext::Freeway);
        assert!(heavy.desired_speed_factor < light.desired_speed_factor);
        assert!(heavy.headway_factor > light.headway_factor);
    }

    #[test]
    fn every_weather_parameter_declares_its_source() {
        for p in fhwa_parameters() {
            if p.source.kind == SourceKind::TodoCalibrate {
                assert!(
                    p.calibration.as_ref().is_some_and(|c| !c.trim().is_empty()),
                    "{} needs a calibration plan",
                    p.name
                );
            }
        }
    }
}
