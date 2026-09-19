//! Pose quantisation — `docs/protocol/vwp-v1.md` §3.2, and the ADR 0008 amendment that
//! corrected it.
//!
//! # The correction that matters
//!
//! ADR 0008 originally said "int16 millimetres on a per-keyframe origin". That is
//! impossible: `i16` millimetres span ±32.767 m, which cannot address a square kilometre
//! from one origin. The amendment and §3.2 correct it, and this module implements the
//! corrected rule:
//!
//! | Quantity | Wire | Scale | Reference |
//! |---|---|---|---|
//! | keyframe x, y | `i32` | millimetres | the run's origin |
//! | keyframe z | `i16` | centimetres | the run's origin |
//! | delta x, y, z | `i16` | millimetres | the **previously transmitted quantised** value |
//! | heading | `u16` | binary radians | absolute |
//! | speed | `i16` | 1/128 m/s | absolute |
//! | acceleration | `i16` | 1/64 m/s² | absolute |
//!
//! # Why the reference is the transmitted value
//!
//! A delta is `q(x_now) − x_transmitted`, never `q(x_now − x_before)`. The difference is
//! the whole point. Differencing the *true* positions and quantising the difference
//! commits a fresh rounding error every step and integrates it: at 13.894 m/s and a
//! 100 ms step each step's true displacement is 1389.4 mm, rounds to 1389, and 10,000
//! steps later the receiver is four metres behind the transmitter. Differencing against
//! the value already on the wire makes the receiver's state *equal* the transmitter's
//! quantised state at every step, so the error is bounded by half a millimetre forever.
//! `tests/drift.rs` measures both.
//!
//! # z, and a unit the specification leaves implicit
//!
//! §3.2 gives delta z in millimetres while §3.3.2 gives keyframe z in centimetres, and
//! §3.4.3's absolute-escape block gives centimetres again. The two are reconciled the
//! only way that keeps "relative to the previously transmitted quantised value" true:
//! the transmitted reference is carried in **millimetres**, and a keyframe or an escape
//! sets it to `z_cm · 10`. A client applying keyframe + deltas therefore tracks z in
//! millimetres, its value re-snaps to the centimetre grid at each keyframe, and writer
//! and reader agree exactly. [`PoseRef`] is that reference.
//!
//! # Rounding
//!
//! `q(v, scale) = clamp(round_half_away_from_zero(v · scale), TYPE_MIN, TYPE_MAX)`.
//! [`f64::round`] *is* half-away-from-zero, and multiplication and rounding are both
//! correctly rounded IEEE-754 operations, so every platform produces the same integer.
//! No transcendental is involved, so nothing here needs [`v2xw_core::math`] — except the
//! heading, whose 2π comes from a constant, not from a library call.

use v2xw_core::time::Duration;

/// Millimetres per metre — the keyframe x/y and delta x/y/z scale (§3.2).
pub const MM_PER_M: f64 = 1000.0;

/// Centimetres per metre — the keyframe and escape-block z scale (§3.2).
pub const CM_PER_M: f64 = 100.0;

/// Binary radians in a full turn (§3.2): `rad = brad · 2π / 65536`.
pub const BRAD_PER_TURN: f64 = 65536.0;

/// Speed scale: `i16` at 1/128 m/s (§3.2).
pub const SPEED_SCALE: f64 = 128.0;

/// Acceleration scale: `i16` at 1/64 m/s² (§3.2).
pub const ACCEL_SCALE: f64 = 64.0;

/// Signal time-to-change scale: `u16` deciseconds (§3.2).
pub const DECISECONDS_PER_S: f64 = 10.0;

/// The largest per-step displacement a delta can carry before the absolute escape is
/// used (§3.2 "Escape hatch"): 32,000 mm, inside `i16::MAX` with room for the sign.
pub const DELTA_ESCAPE_MM: i64 = 32_000;

/// The position grid a decoded pose lands on, in metres — 1 mm, the D9 default for
/// metres. Used by the exporters' grid scan.
pub const Q_POSITION_M: f64 = 1e-3;

/// The heading grid a decoded heading lands on, in radians: one binary radian.
pub const Q_HEADING_RAD: f64 = 1e-6;

/// Rounds half away from zero — the rule §3.2 fixes, spelled once.
///
/// [`f64::round`] already rounds half away from zero; this function exists so the rule
/// is named at every call site and so a reader can check it against the specification
/// without knowing Rust's rounding conventions.
#[inline]
pub fn round_half_away_from_zero(v: f64) -> f64 {
    v.round()
}

/// `q(v, scale)` of §3.2, clamped into `[min, max]`.
///
/// A non-finite input maps to `0`: the pose columns have no float sentinel, and a `NaN`
/// position is a producer bug that must not become an arbitrary integer.
#[inline]
pub fn q(v: f64, scale: f64, min: i64, max: i64) -> i64 {
    if !v.is_finite() {
        return 0;
    }
    let scaled = round_half_away_from_zero(v * scale);
    if scaled <= min as f64 {
        min
    } else if scaled >= max as f64 {
        max
    } else {
        scaled as i64
    }
}

/// Absolute x or y: millimetres relative to the run origin, `i32` (§3.2).
#[inline]
pub fn x_mm(value_m: f64, origin_m: f64) -> i32 {
    q(
        value_m - origin_m,
        MM_PER_M,
        i32::MIN as i64,
        i32::MAX as i64,
    ) as i32
}

/// Absolute z: centimetres relative to the run origin, `i16` (§3.2).
#[inline]
pub fn z_cm(value_m: f64, origin_m: f64) -> i16 {
    q(
        value_m - origin_m,
        CM_PER_M,
        i16::MIN as i64,
        i16::MAX as i64,
    ) as i16
}

/// z on the delta's millimetre grid, used as the transmitted reference (§3.2, and the
/// module note on z).
#[inline]
pub fn z_mm(value_m: f64, origin_m: f64) -> i64 {
    q(value_m - origin_m, MM_PER_M, i64::MIN / 2, i64::MAX / 2)
}

/// Heading in binary radians (§3.2).
///
/// `((round(rad · 65536 / 2π) mod 65536) + 65536) mod 65536` — it wraps by construction,
/// which is the reason for the type: there is no ±π branch to get wrong, and a full turn
/// is exactly representable. A non-finite heading maps to 0.
#[inline]
pub fn heading_brad(rad: f64) -> u16 {
    if !rad.is_finite() {
        return 0;
    }
    let turns = (rad * BRAD_PER_TURN) / core::f64::consts::TAU;
    if !turns.is_finite() {
        return 0;
    }
    let rounded = round_half_away_from_zero(turns);
    // `rem_euclid` on the f64 keeps the reduction exact for the magnitudes a heading can
    // reach and avoids an out-of-range cast for a heading of many turns.
    let wrapped = rounded.rem_euclid(BRAD_PER_TURN);
    // `rem_euclid` can return the divisor itself for an argument a hair below a multiple
    // of it, which is the one way this could produce 65536.
    if wrapped >= BRAD_PER_TURN {
        0
    } else {
        wrapped as u16
    }
}

/// Heading back to radians, quantised to the decoded grid.
#[inline]
pub fn brad_to_rad(brad: u16) -> f64 {
    v2xw_core::math::quantize_to(
        (f64::from(brad) * core::f64::consts::TAU) / BRAD_PER_TURN,
        Q_HEADING_RAD,
    )
}

/// Speed in 1/128 m/s, `i16` (§3.2).
#[inline]
pub fn speed_cq(mps: f64) -> i16 {
    q(mps, SPEED_SCALE, i16::MIN as i64, i16::MAX as i64) as i16
}

/// Longitudinal acceleration in 1/64 m/s², `i16` (§3.2).
#[inline]
pub fn accel_cq(mps2: f64) -> i16 {
    q(mps2, ACCEL_SCALE, i16::MIN as i64, i16::MAX as i64) as i16
}

/// Signal time-to-change in deciseconds, `0xFFFF` for "unknown" (§3.3.3).
#[inline]
pub fn time_to_change_ds(d: Option<Duration>) -> u16 {
    match d {
        None => crate::wire::U16_NONE,
        Some(d) => {
            let ds = q(d.as_secs_f64(), DECISECONDS_PER_S, 0, u16::MAX as i64 - 1);
            ds as u16
        }
    }
}

/// Millimetres back to metres, on the 1 mm grid.
#[inline]
pub fn mm_to_m(mm: i64, origin_m: f64) -> f64 {
    v2xw_core::math::quantize_to(origin_m + (mm as f64) / MM_PER_M, Q_POSITION_M)
}

/// Centimetres back to metres, on the 1 mm grid.
#[inline]
pub fn cm_to_m(cm: i16, origin_m: f64) -> f64 {
    v2xw_core::math::quantize_to(origin_m + f64::from(cm) / CM_PER_M, Q_POSITION_M)
}

/// A slot's pose exactly as it was last put on the wire — the reference every delta is
/// computed against (§3.2 "Delta reference rule").
///
/// Every field is the transmitted integer, not the engine's float. That is the invariant
/// the whole scheme rests on: a client that applies the keyframe and then every delta in
/// order holds precisely these numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoseRef {
    /// Transmitted x, millimetres from the run origin.
    pub x_mm: i32,
    /// Transmitted y, millimetres from the run origin.
    pub y_mm: i32,
    /// Transmitted z, **millimetres** from the run origin (a keyframe sets it to
    /// `z_cm · 10`; see the module note on z).
    pub z_mm: i64,
    /// Transmitted heading.
    pub heading_brad: u16,
    /// Transmitted speed.
    pub speed_cq: i16,
    /// Transmitted acceleration.
    pub accel_cq: i16,
    /// Transmitted state byte (§3.3.4).
    pub state: u8,
    /// Transmitted verified-neighbour count.
    pub verified_neighbors: u8,
    /// Transmitted lane id, `0xFFFFFFFF` when off-lane or withheld.
    pub lane_id: u32,
}

/// What one step of quantisation produced for one slot: the new reference and how it has
/// to be put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaStep {
    /// The new transmitted reference.
    pub next: PoseRef,
    /// `dx`, `dy`, `dz` in millimetres, valid only when [`DeltaStep::absolute`] is false.
    pub d_mm: [i16; 3],
    /// True if the step exceeded [`DELTA_ESCAPE_MM`] on any axis, so the row carries
    /// `MFLAG_ABSOLUTE` and an entry in the absolute block (§3.2, §3.4.3).
    pub absolute: bool,
}

impl PoseRef {
    /// The reference a keyframe row establishes.
    pub fn from_keyframe(x_mm: i32, y_mm: i32, z_cm: i16) -> Self {
        PoseRef {
            x_mm,
            y_mm,
            z_mm: i64::from(z_cm) * 10,
            heading_brad: 0,
            speed_cq: 0,
            accel_cq: 0,
            state: 0,
            verified_neighbors: 0,
            lane_id: crate::wire::U32_NONE,
        }
    }

    /// Quantises a new absolute pose against this reference and says how to transmit it.
    ///
    /// The target integers are computed from the *true* pose, exactly as a keyframe would
    /// compute them; only the transmitted form differs. This is what keeps the error at
    /// half a millimetre however long the run is.
    pub fn step(&self, target_x_mm: i32, target_y_mm: i32, target_z_mm: i64) -> DeltaStep {
        let dx = i64::from(target_x_mm) - i64::from(self.x_mm);
        let dy = i64::from(target_y_mm) - i64::from(self.y_mm);
        let dz = target_z_mm - self.z_mm;
        let absolute =
            dx.abs() > DELTA_ESCAPE_MM || dy.abs() > DELTA_ESCAPE_MM || dz.abs() > DELTA_ESCAPE_MM;
        let next = PoseRef {
            x_mm: target_x_mm,
            y_mm: target_y_mm,
            // An escape transmits z on the centimetre grid, so the reference it leaves
            // behind is the centimetre value, not the millimetre target.
            z_mm: if absolute {
                i64::from(escape_z_cm(target_z_mm)) * 10
            } else {
                target_z_mm
            },
            ..*self
        };
        DeltaStep {
            next,
            d_mm: if absolute {
                [0, 0, 0]
            } else {
                [dx as i16, dy as i16, dz as i16]
            },
            absolute,
        }
    }
}

/// The `z_cm` an absolute-escape block carries for a millimetre target (§3.4.3).
#[inline]
pub fn escape_z_cm(z_mm: i64) -> i16 {
    let cm = round_half_away_from_zero(z_mm as f64 / 10.0);
    if cm <= i16::MIN as f64 {
        i16::MIN
    } else if cm >= i16::MAX as f64 {
        i16::MAX
    } else {
        cm as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example of §9: slot 0 of the keyframe at t = 1 s.
    #[test]
    fn the_specification_example_quantises_as_printed() {
        assert_eq!(x_mm(12.345, -500.0), 512_345);
        assert_eq!(x_mm(-3.210, -500.0), 496_790);
        assert_eq!(z_cm(0.15, 0.0), 15);
        assert_eq!(heading_brad(0.0), 0);
        assert_eq!(speed_cq(13.89), 1778);
        assert_eq!(accel_cq(0.5), 32);
        // Slot 1.
        assert_eq!(heading_brad(core::f64::consts::PI), 32_768);
        assert_eq!(speed_cq(11.0), 1408);
        assert_eq!(accel_cq(-1.2), -77);
        // Slot 2.
        assert_eq!(heading_brad(core::f64::consts::FRAC_PI_2), 16_384);
        assert_eq!(x_mm(3.0, -500.0), 503_000);
        assert_eq!(x_mm(41.25, -500.0), 541_250);
        assert_eq!(z_cm(0.10, 0.0), 10);
    }

    #[test]
    fn headings_wrap_by_construction() {
        assert_eq!(
            heading_brad(core::f64::consts::TAU),
            0,
            "a full turn is zero"
        );
        assert_eq!(heading_brad(-core::f64::consts::FRAC_PI_2), 49_152);
        assert_eq!(heading_brad(3.0 * core::f64::consts::TAU), 0);
        assert_eq!(
            heading_brad(core::f64::consts::PI + core::f64::consts::TAU),
            32_768
        );
        assert_eq!(heading_brad(f64::NAN), 0);
        // Just under a full turn stays just under, it does not overflow to 65536.
        let brad = heading_brad(core::f64::consts::TAU - 1e-9);
        assert_eq!(
            brad, 0,
            "rounding a hair under a turn lands on zero, not 65536"
        );
        assert_eq!(heading_brad(core::f64::consts::TAU - 1e-3), 65_526);
    }

    #[test]
    fn rounding_is_half_away_from_zero() {
        assert_eq!(q(0.5, 1.0, -10, 10), 1);
        assert_eq!(q(-0.5, 1.0, -10, 10), -1);
        assert_eq!(q(1.5, 1.0, -10, 10), 2);
        assert_eq!(q(-1.5, 1.0, -10, 10), -2);
    }

    #[test]
    fn quantities_clamp_rather_than_wrap() {
        assert_eq!(speed_cq(1.0e9), i16::MAX);
        assert_eq!(accel_cq(-1.0e9), i16::MIN);
        assert_eq!(z_cm(1.0e9, 0.0), i16::MAX);
    }

    #[test]
    fn an_over_long_step_asks_for_the_absolute_escape() {
        let r = PoseRef::from_keyframe(0, 0, 0);
        let small = r.step(31_999, 0, 0);
        assert!(!small.absolute);
        assert_eq!(small.d_mm, [31_999, 0, 0]);
        let big = r.step(40_000, 0, 0);
        assert!(big.absolute);
        assert_eq!(big.next.x_mm, 40_000);
    }
}
