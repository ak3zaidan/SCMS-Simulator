//! Table schemas for the exporters, and the declared grid of every float column.
//!
//! # Where a field's quantum comes from
//!
//! D9: "every schema field that carries a float declares its quantum (metres and seconds
//! default to 1e-3, dB to 1e-2, ratios to 1e-4, probabilities to 1e-6, **geodetic degrees
//! to 1e-7**) … the quantum is part of the field's contract and appears in the model card
//! or the dataset schema."
//!
//! [`declared_quantum`] turns that sentence into a function of the field's name, using the
//! unit suffix the naming convention of 03-interfaces §1 already requires (`_m`, `_mps`,
//! `_s`, `_ms`, `_db`, `_dbm`, `_deg`, `_ratio`, …). The result is written into the
//! export's `schema.json`, so the grid is *declared in the artefact* rather than known
//! only to this crate — which is what makes [`crate::export::scan`] a check rather than a
//! tautology.
//!
//! The fallback for a name that carries no unit is 1e-6, the finest grid D9 lists. Erring
//! fine is the safe direction: a value already on a coarser grid is unchanged by it
//! (`crate::grid`'s `metric_grid_composes_with_the_coarser_ones` proves the bit-for-bit
//! claim), while erring coarse would silently destroy precision the field's contract
//! promised.

use std::collections::{BTreeMap, BTreeSet};

use arrow::datatypes::{DataType, Field, Schema};
use serde::{Deserialize, Serialize};

use crate::error::{RecordError, Result};
use crate::grid;

/// The column type an exported table uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    /// A 64-bit signed integer. Unsigned values beyond `i64::MAX` are rejected rather
    /// than silently wrapped.
    Int,
    /// A double, quantised to the column's declared grid at the writer.
    Float,
    /// A UTF-8 string. A nested JSON array or object is stored as its compact JSON text.
    Text,
    /// A boolean.
    Bool,
}

/// One exported column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// The column name, which is the record's JSON key.
    pub name: String,
    /// The column type.
    pub kind: ColumnKind,
    /// The grid a float column is quantised to; `None` for a non-float column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantum: Option<f64>,
    /// True if the column carries ground truth (§5.2), so the `NODE-only` export profile
    /// projects it out.
    pub ground_truth: bool,
}

/// An exported table's schema, and the `schema.json` sidecar's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableSchema {
    /// The schema id, `v2xw/<channel>/1` (08-measurement-and-data.md §5).
    pub schema: String,
    /// The recording channel the rows came from.
    pub channel: String,
    /// The visibility tag of the channel as a whole.
    pub visibility: String,
    /// The columns, in export order.
    pub columns: Vec<ColumnSpec>,
}

/// The column every exported table carries first: the record's simulated time.
pub const TIME_COLUMN: &str = "sim_time_ns";

/// The quantum D9 declares for a field, from its unit suffix.
///
/// The rules, in the order they are tried:
///
/// | Name matches | Quantum | Unit |
/// |---|---|---|
/// | `*_deg`, `*_lat`, `*_lon` | 1e-7 | geodetic degrees |
/// | `*_db`, `*_dbm`, `*_dbi`, `*_dbw` | 1e-2 | decibels |
/// | `*_m`, `*_mps`, `*_mps2`, `*_km`, `*_mm`, `*_cm` | 1e-3 | metres and derived |
/// | `*_s`, `*_ms`, `*_us`, `*_ns`, `*_ppm` | 1e-3 | seconds and derived |
/// | `*_ratio`, `*_frac`, `cbr`, `*_hdop`, `*_pm` | 1e-4 | ratios |
/// | anything else | 1e-6 | probabilities and unnamed units |
pub fn declared_quantum(field: &str) -> f64 {
    let f = field.to_ascii_lowercase();
    let ends = |s: &str| f == s || f.ends_with(&format!("_{s}"));
    if ends("deg") || ends("lat") || ends("lon") || ends("latitude") || ends("longitude") {
        return grid::Q_DEGREES;
    }
    if ends("db") || ends("dbm") || ends("dbi") || ends("dbw") {
        return grid::Q_DB;
    }
    if ends("m") || ends("mps") || ends("mps2") || ends("km") || ends("mm") || ends("cm") {
        return grid::Q_METRES;
    }
    if ends("s") || ends("ms") || ends("us") || ends("ns") || ends("ppm") {
        return grid::Q_SECONDS;
    }
    if ends("ratio") || ends("frac") || ends("cbr") || ends("hdop") || ends("pm") {
        return grid::Q_RATIO;
    }
    grid::Q_PROBABILITY
}

/// True if the field's *name* declares a continuous quantity, whatever values a window
/// happened to hold.
///
/// [`TableSchema::infer`] otherwise types a column by what it saw, and a `cbr` window that
/// happened to contain only `0` and `1` came out as `Int64` with no quantum — which both
/// gave two windows of one channel two different Parquet schemas and dropped the column
/// out of [`crate::export::scan`] entirely, since the scan only looks at float columns.
/// Seeding the kind from the unit suffix fixes both.
///
/// The seconds family (`_s`, `_ms`, `_us`, `_ns`) is deliberately **not** here even though
/// [`declared_quantum`] gives it a grid: a nanosecond or millisecond field is an integer
/// counter far more often than a rate in this project's naming convention, `sim_time_ns`
/// first among them, and forcing those to `Float64` would lose exactness for no gain. Nor
/// are `_mm` and `_cm`, which name the quantised integers of §3.3 rather than a measure.
pub fn declares_float(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    let ends = |s: &str| f == s || f.ends_with(&format!("_{s}"));
    ends("deg")
        || ends("lat")
        || ends("lon")
        || ends("latitude")
        || ends("longitude")
        || ends("db")
        || ends("dbm")
        || ends("dbi")
        || ends("dbw")
        || ends("m")
        || ends("mps")
        || ends("mps2")
        || ends("km")
        || ends("ratio")
        || ends("frac")
        || ends("cbr")
        || ends("hdop")
        || ends("pm")
}

/// Quantises every float inside a JSON value to the grid its innermost key declares.
///
/// The exporters' float columns went through [`v2xw_core::math::quantize_to`] and their
/// *text* columns did not: a JSON object or array is typed [`ColumnKind::Text`] and was
/// written by stringifying it, so every number inside one reached Parquet, Arrow IPC and
/// JSONL with its raw digits, and the scan — which only reads float columns — reported
/// success. This is the gate that closes that, and [`crate::export::scan`] now reads the
/// nested values back and checks them.
///
/// An array takes its key from the array itself (`samples_m` gives every element the metre
/// grid); an object's members take theirs from their own key. A value that will not fit an
/// `f64` JSON number after rounding is left as it was rather than silently nulled.
pub fn quantise_nested(key: &str, value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(n) if n.is_f64() => match n.as_f64() {
            Some(x) => {
                let q = v2xw_core::math::quantize_to(x, declared_quantum(key));
                serde_json::Number::from_f64(q)
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|| value.clone())
            }
            None => value.clone(),
        },
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(|e| quantise_nested(key, e)).collect())
        }
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, e)| (k.clone(), quantise_nested(k, e)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The ground-truth columns of §5.2's event-payload table, by channel.
///
/// §5.2 is exhaustive for v1 and a conformance test enumerates it; this is that list, in
/// the column names the serde records use. A channel whose whole visibility is
/// [`v2xw_core::Visibility::Gt`] does not appear here, because the `NODE-only` profile
/// withholds it entirely rather than column by column.
pub fn ground_truth_fields(channel: &str) -> &'static [&'static str] {
    match channel {
        // Both spellings, deliberately. §5.2 names these columns in prose
        // (`tx_node`, `distance_m`), and the record the engine actually emits — a
        // `#[serde(transparent)]` newtype around `v2xw_metrics::channels::PhyRxView` —
        // spells them `tx` and `dist_m`. This list once held only the prose names, so
        // `without_ground_truth` matched nothing on a real `phy.rx` record and a
        // `NODE-only` export carried the transmitter's identity and the true
        // transmitter-to-receiver distance: the exact leak the whole profile exists to
        // prevent, silently, with the scan and the profile both reporting success.
        // Listing every spelling a producer might use is cheap; a column that is
        // withheld under a name nothing emits is worthless. The test
        // `the_ground_truth_list_matches_the_field_names_records_really_carry` pins the
        // real ones to the view.
        "phy.rx" => &["tx", "tx_node", "dist_m", "distance_m", "los_class"],
        "node.neighbor" => &["peer_actor_id"],
        "det.observation" => &["subject_actor_id"],
        "app.warning" => &["truth", "subject_actor_id"],
        "ma.report" | "ma.case" | "ma.decision" => &["subject_actor_id"],
        "node.telemetry" => &["clock_offset_ns", "pos_error_m"],
        "snapshot.keyframe" | "snapshot.delta" => &["lane_id", "accel_cq", "cause"],
        _ => &[],
    }
}

impl TableSchema {
    /// Infers a schema from the records of one channel.
    ///
    /// The column set is the union of the records' JSON keys, sorted, with
    /// [`TIME_COLUMN`] first. A key whose values are sometimes integral and sometimes
    /// fractional becomes a float column; a key whose values are objects or arrays
    /// becomes a text column holding their compact JSON, whose numbers are quantised by
    /// [`quantise_nested`] and checked by [`crate::export::scan`] like any other.
    ///
    /// Inference is by value **and by name**: a key whose name declares a continuous
    /// quantity ([`declares_float`]) is a float column even in a window whose values all
    /// happened to be integral. Without that, one window of `cbr` holding only `0` and `1`
    /// exported as `Int64` with no declared quantum while the next exported as `Float64` —
    /// two schemas for one channel, and a column the grid scan never looked at.
    ///
    /// Inference rather than a hard-coded field list is deliberate. The record types live
    /// in the family crates, they are still being written, and a table this crate could
    /// not derive from the data would be a second, independently mutable copy of their
    /// field lists — the drift D11's model-card rule exists to prevent. What this crate
    /// *does* fix is the grid, which is a contract rather than a shape.
    ///
    /// # Errors
    /// [`RecordError::Json`] if a record is not a JSON object, and
    /// [`RecordError::UnknownChannel`] if the channel is not in 03-interfaces §14.
    pub fn infer(channel: &str, rows: &[(u64, Vec<u8>)]) -> Result<Self> {
        let spec = crate::channels::by_name(channel)
            .ok_or_else(|| RecordError::UnknownChannel(channel.to_string()))?;
        let gt_fields: BTreeSet<&str> = ground_truth_fields(channel).iter().copied().collect();
        let mut kinds: BTreeMap<String, ColumnKind> = BTreeMap::new();
        for (_, json) in rows {
            let value: serde_json::Value = serde_json::from_slice(json)?;
            let obj = value.as_object().ok_or_else(|| {
                RecordError::malformed(
                    "record",
                    format!("a record on {channel} is not a JSON object"),
                )
            })?;
            for (k, v) in obj {
                let kind = match v {
                    serde_json::Value::Bool(_) => ColumnKind::Bool,
                    serde_json::Value::Number(n) => {
                        if n.is_f64() && !n.is_i64() && !n.is_u64() {
                            ColumnKind::Float
                        } else {
                            ColumnKind::Int
                        }
                    }
                    serde_json::Value::String(_) => ColumnKind::Text,
                    serde_json::Value::Null => continue,
                    _ => ColumnKind::Text,
                };
                kinds
                    .entry(k.clone())
                    .and_modify(|existing| *existing = widen(*existing, kind))
                    .or_insert(kind);
            }
        }
        // A column whose name declares a continuous quantity is a float column whatever
        // this window's values happened to be (see `declares_float`).
        for (name, kind) in kinds.iter_mut() {
            if *kind == ColumnKind::Int && declares_float(name) {
                *kind = ColumnKind::Float;
            }
        }
        let mut columns = Vec::with_capacity(kinds.len() + 1);
        if !kinds.contains_key(TIME_COLUMN) {
            columns.push(ColumnSpec {
                name: TIME_COLUMN.to_string(),
                kind: ColumnKind::Int,
                quantum: None,
                ground_truth: false,
            });
        }
        for (name, kind) in kinds {
            let quantum = (kind == ColumnKind::Float).then(|| declared_quantum(&name));
            columns.push(ColumnSpec {
                ground_truth: gt_fields.contains(name.as_str()),
                name,
                kind,
                quantum,
            });
        }
        Ok(TableSchema {
            schema: format!("v2xw/{channel}/1"),
            channel: channel.to_string(),
            visibility: spec.visibility.to_string(),
            columns,
        })
    }

    /// The same schema with the ground-truth columns removed — the `NODE-only` export
    /// profile, and conformance V6's "GT and NODE never share a file".
    pub fn without_ground_truth(&self) -> Self {
        TableSchema {
            schema: self.schema.clone(),
            channel: self.channel.clone(),
            visibility: format!("{}-node-only", self.visibility),
            columns: self
                .columns
                .iter()
                .filter(|c| !c.ground_truth)
                .cloned()
                .collect(),
        }
    }

    /// The column with this name.
    pub fn column(&self, name: &str) -> Option<&ColumnSpec> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The Arrow schema, with the declared grid carried in each float field's metadata so
    /// a Parquet or Arrow IPC file is self-describing about it too.
    pub fn arrow_schema(&self) -> Schema {
        let fields: Vec<Field> = self
            .columns
            .iter()
            .map(|c| {
                let dt = match c.kind {
                    ColumnKind::Int => DataType::Int64,
                    ColumnKind::Float => DataType::Float64,
                    ColumnKind::Text => DataType::Utf8,
                    ColumnKind::Bool => DataType::Boolean,
                };
                let mut field = Field::new(&c.name, dt, true);
                let mut meta = std::collections::HashMap::new();
                if let Some(q) = c.quantum {
                    meta.insert("v2xw.quantum".to_string(), format_quantum(q));
                }
                meta.insert("v2xw.ground_truth".to_string(), c.ground_truth.to_string());
                field = field.with_metadata(meta);
                field
            })
            .collect();
        let mut meta = std::collections::HashMap::new();
        meta.insert("v2xw.schema".to_string(), self.schema.clone());
        meta.insert("v2xw.channel".to_string(), self.channel.clone());
        meta.insert("v2xw.visibility".to_string(), self.visibility.clone());
        Schema::new(fields).with_metadata(meta)
    }

    /// The `schema.json` sidecar's bytes, with keys sorted so the file is reproducible.
    ///
    /// # Errors
    /// [`RecordError::Json`] if the schema will not serialise.
    pub fn to_json(&self) -> Result<Vec<u8>> {
        let mut out = v2xw_core::canonical_json(self)
            .map_err(|e| RecordError::malformed("schema.json", e.to_string()))?;
        out.push(b'\n');
        Ok(out)
    }

    /// Reads a `schema.json` sidecar back.
    ///
    /// # Errors
    /// [`RecordError::Json`] if the bytes are not a schema.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// A quantum as a short decimal string, e.g. `1e-3`.
fn format_quantum(q: f64) -> String {
    format!("{q:e}")
}

fn widen(a: ColumnKind, b: ColumnKind) -> ColumnKind {
    use ColumnKind::{Bool, Float, Int, Text};
    match (a, b) {
        (x, y) if x == y => x,
        (Int, Float) | (Float, Int) => Float,
        (Bool, Int) | (Int, Bool) => Int,
        _ => Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_convention_of_d9_is_implemented() {
        assert_eq!(declared_quantum("pos_x_m"), 1e-3);
        assert_eq!(declared_quantum("distance_m"), 1e-3);
        assert_eq!(declared_quantum("speed_mps"), 1e-3);
        assert_eq!(declared_quantum("airtime_ms"), 1e-3);
        assert_eq!(declared_quantum("rssi_dbm"), 1e-2);
        assert_eq!(declared_quantum("sinr_db"), 1e-2);
        assert_eq!(declared_quantum("origin_lat_deg"), 1e-7);
        assert_eq!(declared_quantum("cbr"), 1e-4);
        assert_eq!(declared_quantum("gnss_hdop"), 1e-4);
        assert_eq!(declared_quantum("score"), 1e-6);
        assert_eq!(declared_quantum("confidence"), 1e-6);
    }

    #[test]
    fn a_column_seen_as_both_integer_and_fraction_becomes_a_float() {
        let rows = vec![
            (0u64, br#"{"node_id":1,"cbr":0}"#.to_vec()),
            (1u64, br#"{"node_id":2,"cbr":0.25}"#.to_vec()),
        ];
        let s = TableSchema::infer("mac.cbr", &rows).unwrap();
        assert_eq!(s.column("cbr").unwrap().kind, ColumnKind::Float);
        assert_eq!(s.column("cbr").unwrap().quantum, Some(1e-4));
        assert_eq!(s.column("node_id").unwrap().kind, ColumnKind::Int);
        assert_eq!(s.columns[0].name, TIME_COLUMN);
    }

    #[test]
    fn a_window_of_integral_values_still_types_a_unit_bearing_column_as_a_float() {
        // The F12 case: `cbr` happened to hold only 0 and 1 in this window.
        let rows = vec![
            (0u64, br#"{"node_id":1,"cbr":0}"#.to_vec()),
            (1u64, br#"{"node_id":2,"cbr":1}"#.to_vec()),
        ];
        let s = TableSchema::infer("mac.cbr", &rows).unwrap();
        assert_eq!(
            s.column("cbr").unwrap().kind,
            ColumnKind::Float,
            "a ratio column is a float column whatever this window held"
        );
        assert_eq!(s.column("cbr").unwrap().quantum, Some(1e-4));
        // …and a genuine integer column is untouched, the time column above all.
        assert_eq!(s.column("node_id").unwrap().kind, ColumnKind::Int);
        assert_eq!(s.column("node_id").unwrap().quantum, None);
        assert_eq!(s.column(TIME_COLUMN).unwrap().kind, ColumnKind::Int);
        assert!(
            !declares_float("sim_time_ns"),
            "a time counter is an integer"
        );
        assert!(!declares_float("clock_offset_ns"));
        assert!(!declares_float("node_id"));
        assert!(declares_float("distance_m"));
        assert!(declares_float("rssi_dbm"));
        assert!(declares_float("speed_mps"));
        assert!(declares_float("cbr"));
    }

    #[test]
    fn nested_values_are_quantised_by_their_innermost_key() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"detail":{"distance_m":123.4567891},"samples_m":[1.234567891,2.0],"n":7,"s":"x"}"#,
        )
        .expect("json");
        let q = quantise_nested("node.tx", &v);
        assert_eq!(
            q,
            serde_json::from_str::<serde_json::Value>(
                r#"{"detail":{"distance_m":123.457},"samples_m":[1.235,2.0],"n":7,"s":"x"}"#
            )
            .expect("json")
        );
    }

    #[test]
    fn the_node_only_profile_projects_out_the_ground_truth_columns() {
        let rows = vec![(
            0u64,
            br#"{"rx_node":1,"tx_node":2,"distance_m":10.5,"rssi_dbm":-70.25}"#.to_vec(),
        )];
        let s = TableSchema::infer("phy.rx", &rows).unwrap();
        assert!(s.column("tx_node").unwrap().ground_truth);
        assert!(s.column("distance_m").unwrap().ground_truth);
        assert!(!s.column("rssi_dbm").unwrap().ground_truth);
        let blind = s.without_ground_truth();
        assert!(blind.column("tx_node").is_none());
        assert!(blind.column("distance_m").is_none());
        assert!(blind.column("rssi_dbm").is_some());
    }
}
