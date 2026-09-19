//! Simulator quantities to ETSI CDD wire units.
//!
//! Every ETSI data element is an integer on a declared grid with declared sentinels, and
//! every one of these functions is written against the exact wording of its `@unit` and
//! value-list comment in `ETSI-ITS-CDD.asn` (TS 102 894-2 Release 2). The file is in the
//! repository at `third_party/asn1/etsi/cdd_ts102894_2/ETSI-ITS-CDD.asn`, so a reviewer can
//! check a claim here against the line it came from.
//!
//! # Three rules, applied everywhere
//!
//! 1. **Quantise before scaling** (build decisions D9 and D10). Every incoming float goes
//!    through [`v2xw_core::math::quantize_to`] on its declared grid before it is divided by
//!    the ASN.1 unit. D10's measured justification is that a one-ulp difference in a
//!    transcendental shifted one legacy run's report count by 1.7 %; the same effect at a
//!    rounding boundary would flip an encoded integer by one and change the message bytes.
//!    Quantising first makes the integer a function of the quantised value, not of the last
//!    bit of an f64.
//! 2. **Saturate to the standard's own sentinels, do not fail.** The CDD gives every
//!    bounded element an `outOfRange` and an `unavailable` value, and a real ITS-S emits
//!    them rather than refusing to send. A speed of 200 m/s encodes as `outOfRange`, a NaN
//!    or a missing measurement as `unavailable`. Only values that cannot be interpreted at
//!    all — a latitude outside ±90° — are an error, because those mean the caller's frame
//!    of reference is wrong and encoding a sentinel would hide it.
//! 3. **Round the way the element's own comment says.** Most CDD magnitude elements say
//!    "`n` … if the value is equal to or less than n × unit and greater than (n-1) × unit",
//!    which is a *ceiling*, not a nearest-value rounding; [`ceil_units`] implements it and
//!    the functions that need it say so. Elements that are a plain change of scale
//!    (latitude, longitude, altitude, heading, yaw rate) use [`round_units`].
//!
//! `f64::round`, `ceil`, `floor` and `abs` are IEEE-754 operations with exactly-specified
//! results, not library transcendentals, so ADR 0003's ban on `std` maths does not reach
//! them — and [`v2xw_core::math`] deliberately does not wrap them.

use v2xw_core::belief::PositionEstimate;
use v2xw_core::geo::GeoOrigin;
use v2xw_core::math;

// The grids. The four that cross a crate boundary are **aliases of the core crate's own
// constants**, not fresh literals: D9 says there is one table, and two tables that happen
// to agree today are a table that will disagree later.

/// Quantum applied to any length or position before scaling, metres.
pub const Q_M: f64 = PositionEstimate::Q_M;
/// Quantum applied to any speed before scaling, m/s.
///
/// The same 1e-3 as [`Q_M`], and deliberately the same constant:
/// [`PositionEstimate::Q_M`] documents itself as the grid "for metres and metres per
/// second", which is D9's "metres and seconds default to 1e-3".
pub const Q_MPS: f64 = PositionEstimate::Q_M;
/// Quantum applied to any angle before scaling, radians.
pub const Q_RAD: f64 = PositionEstimate::Q_RAD;
/// Quantum applied to a geodetic angle before scaling, degrees.
///
/// 1e-7° is 11 mm of latitude and is exactly the resolution of the 1/10-microdegree
/// `Latitude` and `Longitude` a CAM carries, which is why D9 gives degrees their own grid.
pub const Q_DEG: f64 = GeoOrigin::Q_DEG;
/// Quantum applied to any acceleration before scaling, m/s².
///
/// Declared here rather than in the core crate because no core type carries an
/// acceleration; 1e-3 follows D9's rule for metres and seconds.
pub const Q_MPS2: f64 = 1e-3;
/// Quantum applied to any angular rate before scaling, rad/s. Follows [`Q_RAD`].
pub const Q_RAD_S: f64 = PositionEstimate::Q_RAD;
/// Quantum applied to a curvature before scaling, m⁻¹.
///
/// 1e-6 m⁻¹ is a 1,000,000 m radius: four orders of magnitude finer than the
/// `CurvatureValue` grid it feeds, so the quantisation never moves the encoded integer.
pub const Q_INV_M: f64 = 1e-6;

/// Radians to degrees. A multiplication by a constant — no transcendental involved.
pub fn rad_to_deg(rad: f64) -> f64 {
    rad * (180.0 / core::f64::consts::PI)
}

/// Scales to wire units with round-half-away-from-zero, for elements that are a plain
/// change of scale.
pub fn round_units(value: f64, unit: f64) -> f64 {
    (value / unit).round()
}

/// Scales to wire units with a ceiling, for elements whose CDD comment reads "`n` … if the
/// value is equal to or less than n × unit and greater than (n-1) × unit".
pub fn ceil_units(value: f64, unit: f64) -> f64 {
    (value / unit).ceil()
}

/// Clamps a scaled value into `[min, max]`, mapping a non-finite input to `unavailable`.
fn clamp_or(scaled: f64, min: i64, max: i64, unavailable: i64) -> i64 {
    if !scaled.is_finite() {
        return unavailable;
    }
    // `as i64` saturates at the extremes in Rust 2021+ float-to-int casts, and the clamp
    // then puts it inside the ASN.1 range, so no value can escape.
    (scaled as i64).clamp(min, max)
}

/// ENU heading (radians, 0 = east, counter-clockwise — build decision D6) to a WGS84
/// compass bearing in degrees, `0 = north`, clockwise, in `[0, 360)`.
///
/// The engine's own convention is the legacy one and is used everywhere inside the
/// simulator; ETSI's `HeadingValue` and `Wgs84AngleValue` are the other convention, and
/// this is the only place the two meet.
pub fn enu_heading_to_wgs84_bearing_deg(heading_rad: f64) -> f64 {
    let enu_deg = rad_to_deg(math::quantize_to(heading_rad, Q_RAD));
    let bearing = 90.0 - enu_deg;
    // `rem_euclid` is exact for finite inputs and always lands in [0, 360).
    if bearing.is_finite() {
        bearing.rem_euclid(360.0)
    } else {
        f64::NAN
    }
}

// ---------------------------------------------------------------------------------------
// Position
// ---------------------------------------------------------------------------------------

/// `Latitude`, 1/10 microdegree, `unavailable(900000001)`, range `-900000000..900000001`.
///
/// Returns `None` when `deg` is outside ±90°, which means the caller's projection is wrong
/// rather than its sensor being unavailable.
pub fn latitude(deg: f64) -> Option<i32> {
    if !deg.is_finite() || !(-90.0..=90.0).contains(&deg) {
        return None;
    }
    let scaled = round_units(math::quantize_to(deg, Q_DEG), 1e-7);
    Some(clamp_or(scaled, -900_000_000, 900_000_000, 900_000_001) as i32)
}

/// `Longitude`, 1/10 microdegree, `unavailable(1800000001)`, range `-1800000000..1800000001`.
///
/// Returns `None` when `deg` is outside ±180°.
pub fn longitude(deg: f64) -> Option<i32> {
    if !deg.is_finite() || !(-180.0..=180.0).contains(&deg) {
        return None;
    }
    let scaled = round_units(math::quantize_to(deg, Q_DEG), 1e-7);
    Some(clamp_or(scaled, -1_800_000_000, 1_800_000_000, 1_800_000_001) as i32)
}

/// `AltitudeValue`, 0.01 m, `negativeOutOfRange(-100000)`, `postiveOutOfRange(800000)`,
/// `unavailable(800001)` (upstream's spelling of "positive" is preserved).
pub fn altitude(m: f64) -> i32 {
    if !m.is_finite() {
        return 800_001;
    }
    let scaled = round_units(math::quantize_to(m, Q_M), 0.01);
    clamp_or(scaled, -100_000, 800_000, 800_001) as i32
}

/// `SemiAxisLength`, 0.01 m, `doNotUse(0)`, `outOfRange(4094)`, `unavailable(4095)`.
///
/// A no-fix [`v2xw_core::belief::PositionEstimate`] carries an infinite semi-axis, which
/// lands on `unavailable` — exactly what an ITS-S with no fix should send.
pub fn semi_axis_length(m: f64) -> u16 {
    if !m.is_finite() || m < 0.0 {
        return 4095;
    }
    let scaled = ceil_units(math::quantize_to(m, Q_M), 0.01);
    // 0 is `doNotUse`, so a genuinely zero uncertainty (the `perfect` GNSS model) encodes
    // as 1, the smallest expressible non-zero length.
    clamp_or(scaled.max(1.0), 1, 4094, 4095) as u16
}

/// `Wgs84AngleValue`, 0.1 degree, `doNotUse(3600)`, `unavailable(3601)`, range `0..3601`.
///
/// Takes an ENU heading in radians and converts the convention as well as the unit.
pub fn wgs84_angle(heading_rad: f64) -> u16 {
    let bearing = enu_heading_to_wgs84_bearing_deg(heading_rad);
    if !bearing.is_finite() {
        return 3601;
    }
    let scaled = round_units(bearing, 0.1);
    // 3600 is `doNotUse`; 360.0° and 0.0° are the same bearing, so fold it onto 0.
    let v = clamp_or(scaled, 0, 3600, 3601);
    if v >= 3600 { 0 } else { v as u16 }
}

/// `HeadingValue`, 0.1 degree, same convention and sentinels as [`wgs84_angle`].
pub fn heading_value(heading_rad: f64) -> u16 {
    wgs84_angle(heading_rad)
}

/// `HeadingConfidence`, 0.1 degree, range `1..127`, `outOfRange(126)`, `unavailable(127)`.
///
/// Ceiling, per the element's own wording.
pub fn heading_confidence(accuracy_rad: Option<f64>) -> u8 {
    let Some(rad) = accuracy_rad else { return 127 };
    if !rad.is_finite() || rad < 0.0 {
        return 127;
    }
    let deg = rad_to_deg(math::quantize_to(rad, Q_RAD));
    let scaled = ceil_units(deg, 0.1).max(1.0);
    if scaled > 125.0 { 126 } else { scaled as u8 }
}

// ---------------------------------------------------------------------------------------
// Kinematics
// ---------------------------------------------------------------------------------------

/// `SpeedValue`, 0.01 m/s, `standstill(0)`, `outOfRange(16382)`, `unavailable(16383)`.
///
/// Ceiling: "`n` (`n > 0` and `n < 16 382`) if the applicable value is equal to or less
/// than n × 0,01 m/s, and greater than (n-1) × 0,01 m/s".
pub fn speed_value(mps: f64) -> u16 {
    if !mps.is_finite() || mps < 0.0 {
        return 16383;
    }
    let scaled = ceil_units(math::quantize_to(mps, Q_MPS), 0.01);
    clamp_or(scaled, 0, 16382, 16383) as u16
}

/// `SpeedConfidence`, 0.01 m/s, range `1..127`, `outOfRange(126)`, `unavailable(127)`.
pub fn speed_confidence(accuracy_mps: Option<f64>) -> u8 {
    let Some(mps) = accuracy_mps else { return 127 };
    if !mps.is_finite() || mps < 0.0 {
        return 127;
    }
    let scaled = ceil_units(math::quantize_to(mps, Q_MPS), 0.01).max(1.0);
    if scaled > 125.0 { 126 } else { scaled as u8 }
}

/// `AccelerationValue`, 0.1 m/s², `negativeOutOfRange(-160)`, `positiveOutOfRange(160)`,
/// `unavailable(161)`.
pub fn acceleration_value(mps2: Option<f64>) -> i16 {
    let Some(a) = mps2 else { return 161 };
    if !a.is_finite() {
        return 161;
    }
    let scaled = round_units(math::quantize_to(a, Q_MPS2), 0.1);
    clamp_or(scaled, -160, 160, 161) as i16
}

/// `AccelerationConfidence`, 0.1 m/s², range `0..102`, `outOfRange(101)`,
/// `unavailable(102)`.
pub fn acceleration_confidence(accuracy_mps2: Option<f64>) -> u8 {
    let Some(a) = accuracy_mps2 else { return 102 };
    if !a.is_finite() || a < 0.0 {
        return 102;
    }
    let scaled = ceil_units(math::quantize_to(a, Q_MPS2), 0.1);
    if scaled > 100.0 { 101 } else { scaled as u8 }
}

/// `YawRateValue`, 0.01 degree/s, `negativeOutOfRange(-32766)`,
/// `positiveOutOfRange(32766)`, `unavailable(32767)`.
///
/// Takes rad/s, which is the engine's unit. The sign conventions already agree: the CDD
/// makes a positive value anti-clockwise ("to the left"), and the engine's ENU headings
/// increase counter-clockwise (D6), so a left turn is positive in both. No sign flip.
pub fn yaw_rate_value(rad_per_s: Option<f64>) -> i16 {
    let Some(r) = rad_per_s else { return 32767 };
    if !r.is_finite() {
        return 32767;
    }
    let deg_s = rad_to_deg(math::quantize_to(r, Q_RAD_S));
    let scaled = round_units(deg_s, 0.01);
    clamp_or(scaled, -32766, 32766, 32767) as i16
}

/// `CurvatureValue`, 1/m scaled so that `n` means a radius of `10000/n` metres.
///
/// CDD: `outOfRangeNegative(-1023)`, `straight(0)`, `outOfRangePositive(1022)`,
/// `unavailable(1023)`; the unit comment is "1 over 10 000 metres". A curvature of
/// `1/r` m⁻¹ therefore encodes as `round(10000 · curvature)`, positive to the left.
pub fn curvature_value(inv_m: Option<f64>) -> i16 {
    let Some(c) = inv_m else { return 1023 };
    if !c.is_finite() {
        return 1023;
    }
    let scaled = round_units(math::quantize_to(c, Q_INV_M), 1.0 / 10_000.0);
    clamp_or(scaled, -1023, 1022, 1023) as i16
}

/// `VehicleLengthValue`, 0.1 m, range `1..1023`, `outOfRange(1022)`, `unavailable(1023)`.
pub fn vehicle_length_value(m: f64) -> u16 {
    if !m.is_finite() || m <= 0.0 {
        return 1023;
    }
    let scaled = ceil_units(math::quantize_to(m, Q_M), 0.1).max(1.0);
    if scaled > 1021.0 { 1022 } else { scaled as u16 }
}

/// `VehicleWidth`, 0.1 m, range `1..62`, `outOfRange(61)`, `unavailable(62)`.
pub fn vehicle_width(m: f64) -> u8 {
    if !m.is_finite() || m <= 0.0 {
        return 62;
    }
    let scaled = ceil_units(math::quantize_to(m, Q_M), 0.1).max(1.0);
    if scaled > 60.0 { 61 } else { scaled as u8 }
}

// ---------------------------------------------------------------------------------------
// Deltas (DENM traces, CAM path history)
// ---------------------------------------------------------------------------------------

/// `DeltaLatitude`/`DeltaLongitude`, 1/10 microdegree, `unavailable(131072)`, range
/// `-131071..131072`.
pub fn delta_degrees(delta_deg: f64) -> i32 {
    if !delta_deg.is_finite() {
        return 131_072;
    }
    let scaled = round_units(math::quantize_to(delta_deg, Q_DEG), 1e-7);
    // Out of range is not distinguishable from unavailable for this element; the CDD gives
    // it only the one sentinel, so a delta too large to express becomes `unavailable`.
    if !(-131_071.0..=131_071.0).contains(&scaled) {
        return 131_072;
    }
    scaled as i32
}

/// `DeltaAltitude`, 0.01 m, `negativeOutOfRange(-12700)`, `positiveOutOfRange(12799)`,
/// `unavailable(12800)`.
pub fn delta_altitude(delta_m: f64) -> i16 {
    if !delta_m.is_finite() {
        return 12_800;
    }
    let scaled = round_units(math::quantize_to(delta_m, Q_M), 0.01);
    clamp_or(scaled, -12_700, 12_799, 12_800) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_conversion_turns_enu_into_a_compass_bearing() {
        // ENU 0 rad is east, which is a bearing of 90°, which is HeadingValue 900.
        assert_eq!(heading_value(0.0), 900);
        // ENU π/2 is north → bearing 0 → 0.
        assert_eq!(heading_value(core::f64::consts::FRAC_PI_2), 0);
        // ENU π is west → bearing 270 → 2700.
        assert_eq!(heading_value(core::f64::consts::PI), 2700);
        // ENU -π/2 is south → bearing 180 → 1800.
        assert_eq!(heading_value(-core::f64::consts::FRAC_PI_2), 1800);
        // Every output is inside the ASN.1 range and never the `doNotUse` value.
        for i in -720..=720 {
            let v = heading_value(f64::from(i) * 0.01);
            assert!(v < 3600, "heading {i} produced {v}");
        }
    }

    #[test]
    fn speed_uses_the_ceiling_rule_the_cdd_states() {
        assert_eq!(speed_value(0.0), 0, "standstill");
        assert_eq!(speed_value(0.005), 1, "0,005 m/s is in bucket 1");
        assert_eq!(speed_value(0.01), 1, "exactly n x 0,01 stays in bucket n");
        assert_eq!(speed_value(0.011), 2);
        assert_eq!(speed_value(13.89), 1389, "50 km/h");
        assert_eq!(speed_value(1000.0), 16382, "outOfRange");
        assert_eq!(speed_value(f64::NAN), 16383, "unavailable");
        assert_eq!(speed_value(-1.0), 16383, "a negative speed is not a speed");
    }

    #[test]
    fn position_refuses_an_impossible_frame_but_saturates_a_big_altitude() {
        assert_eq!(latitude(40.75), Some(407_500_000));
        assert_eq!(longitude(-73.98), Some(-739_800_000));
        assert_eq!(latitude(91.0), None);
        assert_eq!(longitude(181.0), None);
        assert_eq!(latitude(f64::NAN), None);
        assert_eq!(altitude(12.34), 1234);
        assert_eq!(altitude(-2000.0), -100_000, "negativeOutOfRange");
        assert_eq!(altitude(f64::INFINITY), 800_001, "unavailable");
    }

    #[test]
    fn an_infinite_error_ellipse_is_unavailable_and_a_zero_one_is_the_smallest_length() {
        assert_eq!(semi_axis_length(f64::INFINITY), 4095);
        assert_eq!(semi_axis_length(0.0), 1, "doNotUse(0) is never emitted");
        assert_eq!(semi_axis_length(1.5), 150);
        assert_eq!(semi_axis_length(100.0), 4094, "outOfRange");
    }

    #[test]
    fn acceleration_and_yaw_rate_saturate_at_the_named_sentinels() {
        assert_eq!(acceleration_value(Some(0.0)), 0);
        assert_eq!(acceleration_value(Some(-3.5)), -35);
        assert_eq!(acceleration_value(Some(-20.0)), -160, "negativeOutOfRange");
        assert_eq!(acceleration_value(None), 161, "unavailable");
        assert_eq!(yaw_rate_value(None), 32767);
        assert_eq!(yaw_rate_value(Some(0.0)), 0);
        // 1 rad/s is 57,2958 deg/s, which is 5730 hundredths (5729.58 rounds to 5730).
        assert_eq!(yaw_rate_value(Some(1.0)), 5730);
    }

    #[test]
    fn dimensions_use_the_ceiling_rule_and_saturate() {
        assert_eq!(vehicle_length_value(4.5), 45);
        assert_eq!(vehicle_width(1.8), 18);
        assert_eq!(vehicle_length_value(0.0), 1023, "unavailable");
        assert_eq!(vehicle_width(10.0), 61, "outOfRange");
    }

    /// D9/D10: the encoded integer must be a function of the quantised value, so two
    /// inputs that quantise to the same grid point must encode identically even when their
    /// raw f64s differ.
    #[test]
    fn inputs_that_quantise_together_encode_together() {
        let a = 13.8890000001_f64;
        let b = 13.8889999999_f64;
        assert_ne!(a, b);
        assert_eq!(speed_value(a), speed_value(b));
        assert_eq!(latitude(40.750000000004), latitude(40.749999999996));
    }

    #[test]
    fn curvature_is_ten_thousand_over_the_radius() {
        assert_eq!(curvature_value(Some(0.0)), 0, "straight");
        // A 100 m radius is a curvature of 0,01 m^-1, which is 100 in wire units.
        assert_eq!(curvature_value(Some(0.01)), 100);
        assert_eq!(curvature_value(Some(-0.01)), -100);
        assert_eq!(curvature_value(None), 1023);
        assert_eq!(curvature_value(Some(1.0)), 1022, "outOfRangePositive");
    }

    #[test]
    fn deltas_fall_back_to_unavailable_when_they_cannot_be_expressed() {
        assert_eq!(delta_degrees(0.0), 0);
        assert_eq!(delta_degrees(0.0001), 1000);
        assert_eq!(delta_degrees(1.0), 131_072, "too far to express");
        assert_eq!(delta_altitude(1.0), 100);
        assert_eq!(delta_altitude(f64::NAN), 12_800);
    }
}
