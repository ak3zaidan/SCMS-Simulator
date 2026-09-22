//! Declared grids, and the scan that enforces them — ADR 0004 §7 and build decision D9.
//!
//! > No floating-point value reaches a recorded, exported or digested artefact in raw
//! > IEEE-754 form. One central writer-side encoder quantises each float to its field's
//! > declared grid, and a scanning test fails the build if any output value sits off its
//! > grid.
//!
//! Most of VWP v1 satisfies this by construction rather than by rounding: a pose is
//! `i32` millimetres, a heading is a `u16`, a speed is an `i16` at 1/128 m/s. Integers
//! have no grid question. What is left is small and is listed here: `Hello`'s geodetic
//! origin and bounding box, `Hello`'s `f32` node positions and class dimensions,
//! `Telemetry`'s ten `f32` fields, and `MetricSample.value`. [`scan_frame`] is the
//! predicate ADR 0004 asks for, applied to a frame rather than to a file, so the recorder
//! can refuse an off-grid frame at the moment of writing instead of at the end of the run.
//!
//! # `f32` and a decimal grid
//!
//! An `f32` cannot sit exactly on a decimal grid in `f64` terms: `1.8f32` widens to
//! 1.79999995…. The grid predicate for a narrowed field is therefore the idempotence of
//! its own encoder — `quantise_f32(v, q) == v` bit for bit — which is what
//! [`is_on_grid_f32`] checks and what `v2xw-world` already does for the world payload.

use v2xw_core::math::{is_on_grid, quantize_to};

use crate::error::{RecordError, Result};
use crate::wire::{Frame, MsgType};

/// Metres and seconds: the legacy 1e-3 convention (D9).
pub const Q_METRES: f64 = 1e-3;
/// Seconds, same grid as metres (D9).
pub const Q_SECONDS: f64 = 1e-3;
/// Decibels: 1e-2 (D9).
pub const Q_DB: f64 = 1e-2;
/// Dimensionless ratios: 1e-4 (D9).
pub const Q_RATIO: f64 = 1e-4;
/// Probabilities: 1e-6 (D9).
pub const Q_PROBABILITY: f64 = 1e-6;
/// Geodetic degrees: 1e-7, about 11 mm of latitude (D9,
/// [`v2xw_core::GeoOrigin::Q_DEG`]).
pub const Q_DEGREES: f64 = v2xw_core::GeoOrigin::Q_DEG;

/// `MetricSample.value` (§3.7).
///
/// The wire carries no quantum, so this crate declares one for the field: 1e-6, the
/// finest grid D9 lists. A provider whose `MetricDef` declares a coarser grid quantises
/// to it first; re-quantising to a grid that divides it is a bit-for-bit no-op, which
/// `metric_grid_composes_with_the_coarser_ones` checks.
pub const Q_METRIC_VALUE: f64 = 1e-6;

/// Quantises to `quantum` and narrows to `f32` — the writer-side encoder for a narrowed
/// field.
pub fn quantise_f32(value: f64, quantum: f64) -> f32 {
    quantize_to(value, quantum) as f32
}

/// True if `value` is already exactly what [`quantise_f32`] would produce for it.
pub fn is_on_grid_f32(value: f32, quantum: f64) -> bool {
    !value.is_finite() || quantise_f32(f64::from(value), quantum).to_bits() == value.to_bits()
}

fn check(ok: bool, channel: &str, field: &str, value: f64, quantum: f64, row: usize) -> Result<()> {
    check_in("<frame>", ok, channel, field, value, quantum, row)
}

fn check_in(
    file: &str,
    ok: bool,
    channel: &str,
    field: &str,
    value: f64,
    quantum: f64,
    row: usize,
) -> Result<()> {
    if ok {
        return Ok(());
    }
    Err(RecordError::OffGrid {
        file: file.to_string(),
        channel: channel.to_string(),
        field: field.to_string(),
        value,
        quantum,
        row,
    })
}

/// Scans every float in a serde record's JSON against its field's declared grid.
///
/// The record path is the other half of D9 and had no gate at all: `write_frame` scanned
/// its frame and `write_record` stored the JSON verbatim, so the crate's own fixture wrote
/// seven raw doubles into a recording and [`crate::reader::Reader::content_digest`] hashed
/// them. The exporters quantise on the way out, which hid it from `export::scan` — but the
/// recording is itself "a recorded artefact", and a replay, a re-export and a run digest
/// are all taken from it.
///
/// The grid of each field is [`crate::export::schema::declared_quantum`] of its key, which
/// is the same function the exporters and the `schema.json` sidecar use, so one contract
/// governs the record wherever it is read. Nested objects and arrays are walked, naming
/// the grid from the innermost key; a JSON integer is on every decimal grid and is not
/// checked.
///
/// # Errors
/// [`RecordError::OffGrid`] naming the first field that is off its grid, or
/// [`RecordError::Json`] if the bytes are not JSON.
pub fn scan_record(channel: &str, json: &[u8]) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(json)?;
    scan_json(channel, "", &value)
}

fn scan_json(channel: &str, key: &str, value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Number(n) => {
            // `is_f64` is false for a JSON integer, which sits on every decimal grid and
            // has no quantisation question; only a fractional literal is checked.
            if n.is_f64() {
                if let Some(x) = n.as_f64() {
                    let q = crate::export::schema::declared_quantum(key);
                    check_in("<record>", is_on_grid(x, q), channel, key, x, q, 0)?;
                }
            }
        }
        serde_json::Value::Array(a) => {
            for e in a {
                scan_json(channel, key, e)?;
            }
        }
        serde_json::Value::Object(m) => {
            for (k, e) in m {
                scan_json(channel, k, e)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Scans every float in a VWP frame against its declared grid.
///
/// Frames that carry no float at all — `Keyframe`, `Delta`, `Event`, `Provenance` — pass
/// trivially, and that is the point: the quantisation question does not arise for them
/// because the layout has no float in it.
///
/// # Errors
/// [`RecordError::OffGrid`] naming the first field that is off its grid, or whatever the
/// frame's decoder returns for a malformed body.
pub fn scan_frame(frame: &Frame) -> Result<()> {
    let header = frame.header()?;
    match header.kind() {
        Some(MsgType::Hello) => {
            let h = crate::wire::hello::HelloBody::decode(frame.body())?;
            check(
                is_on_grid(h.origin_lat_deg, Q_DEGREES),
                "manifest",
                "origin_lat_deg",
                h.origin_lat_deg,
                Q_DEGREES,
                0,
            )?;
            check(
                is_on_grid(h.origin_lon_deg, Q_DEGREES),
                "manifest",
                "origin_lon_deg",
                h.origin_lon_deg,
                Q_DEGREES,
                0,
            )?;
            check(
                is_on_grid(h.origin_alt_m, Q_METRES),
                "manifest",
                "origin_alt_m",
                h.origin_alt_m,
                Q_METRES,
                0,
            )?;
            for (i, v) in h.bbox_m.iter().enumerate() {
                check(
                    is_on_grid(*v, Q_METRES),
                    "manifest",
                    "bbox_m",
                    *v,
                    Q_METRES,
                    i,
                )?;
            }
            for (i, n) in h.nodes.iter().enumerate() {
                for (axis, v) in n.pos_m.iter().enumerate() {
                    check(
                        is_on_grid_f32(*v, Q_METRES),
                        "manifest",
                        ["pos_x_m", "pos_y_m", "pos_z_m"][axis],
                        f64::from(*v),
                        Q_METRES,
                        i,
                    )?;
                }
            }
            for (i, c) in h.classes.iter().enumerate() {
                for (name, v) in [
                    ("length_m", c.length_m),
                    ("width_m", c.width_m),
                    ("height_m", c.height_m),
                ] {
                    check(
                        is_on_grid_f32(v, Q_METRES),
                        "manifest",
                        name,
                        f64::from(v),
                        Q_METRES,
                        i,
                    )?;
                }
            }
            Ok(())
        }
        Some(MsgType::Telemetry) => {
            let t = crate::wire::telemetry::TelemetryBody::decode(frame.body())?;
            for (i, r) in t.records.iter().enumerate() {
                for (name, value, quantum) in r.f32_fields() {
                    check(
                        is_on_grid_f32(value, quantum),
                        "node.telemetry",
                        name,
                        f64::from(value),
                        quantum,
                        i,
                    )?;
                }
            }
            Ok(())
        }
        Some(MsgType::MetricSample) => {
            let m = crate::wire::metric::MetricBody::decode(frame.body())?;
            for (i, r) in m.samples.iter().enumerate() {
                check(
                    is_on_grid(r.value, Q_METRIC_VALUE),
                    "metric.sample",
                    "value",
                    r.value,
                    Q_METRIC_VALUE,
                    i,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value already on a coarser declared grid stays bit-identical when the metric
    /// field's own 1e-6 grid is applied to it, so a provider that quantised to its
    /// `MetricDef`'s grid is not perturbed by the field's.
    #[test]
    fn metric_grid_composes_with_the_coarser_ones() {
        // A fixed deterministic sweep, not a random one: the claim has to hold for the
        // same numbers on every run and every platform.
        let mut x: i64 = 1;
        for _ in 0..20_000 {
            // A small LCG over a wide range of magnitudes.
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let v = (x >> 11) as f64 / (1_i64 << 52) as f64 * 1.0e4;
            for coarse in [Q_METRES, Q_RATIO, Q_DB, Q_PROBABILITY] {
                let a = quantize_to(v, coarse);
                let b = quantize_to(a, Q_METRIC_VALUE);
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "{v} on the {coarse} grid moved when the 1e-6 grid was applied"
                );
            }
        }
    }

    #[test]
    fn the_record_scan_walks_nested_values() {
        // Flat, on grid.
        scan_record("phy.rx", br#"{"distance_m":123.457}"#).expect("on the 1 mm grid");
        // Flat, off grid.
        let err = scan_record("phy.rx", br#"{"distance_m":123.4567891}"#)
            .expect_err("1e-3 is the declared grid for a `_m` field");
        assert!(matches!(err, RecordError::OffGrid { .. }), "got {err}");
        // Nested in an object, named by the innermost key.
        let err = scan_record("node.tx", br#"{"detail":{"distance_m":123.4567891}}"#)
            .expect_err("a nested float is still a recorded float");
        match err {
            RecordError::OffGrid { field, quantum, .. } => {
                assert_eq!(field, "distance_m");
                assert_eq!(quantum, Q_METRES);
            }
            other => panic!("expected OffGrid, got {other}"),
        }
        // Nested in an array, named by the array's key.
        let err = scan_record("node.tx", br#"{"samples_m":[1.234,2.345678912]}"#)
            .expect_err("an array element is still a recorded float");
        assert!(matches!(err, RecordError::OffGrid { .. }), "got {err}");
        scan_record("node.tx", br#"{"samples_m":[1.234,2.346]}"#).expect("both on grid");
        // An integer is on every decimal grid, and a string is not a float.
        scan_record(
            "mac.cbr",
            br#"{"node_id":7,"detector":"plausibility/speed"}"#,
        )
        .expect("no float to check");
    }

    #[test]
    fn narrowed_floats_are_judged_by_their_own_encoder() {
        assert!(
            is_on_grid_f32(1.8, Q_METRES),
            "1.8f32 is what the encoder produces"
        );
        assert!(is_on_grid_f32(2.55, Q_METRES));
        assert!(
            is_on_grid_f32(f32::NAN, Q_METRES),
            "a sentinel is on every grid"
        );
        assert!(!is_on_grid_f32(1.800_04, Q_METRES));
        assert_eq!(quantise_f32(1.800_04, Q_METRES), 1.8);
    }
}
