//! Turning recorded serde records into a columnar batch — the single writer-side
//! quantisation gate every tabular export goes through (ADR 0004 §7, D9).
//!
//! Parquet and Arrow IPC share this code, so there is exactly one place where a float
//! becomes a stored value and exactly one place that has to be right.

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, RecordBatch, StringBuilder,
};

use super::schema::{ColumnKind, TIME_COLUMN, TableSchema};
use crate::error::{RecordError, Result};

/// Builds one Arrow batch from a channel's rows, quantising every float column to its
/// declared grid.
///
/// A row is `(sim_time_ns, json)`, which is what [`crate::reader::Reader::records`]
/// yields. A column the record does not carry is null; a column the schema does not
/// declare is dropped, which is what makes the `NODE-only` projection a schema operation
/// rather than a second code path.
///
/// # Errors
/// [`RecordError::Json`] if a record is not a JSON object,
/// [`RecordError::Unrepresentable`] for an unsigned integer beyond `i64::MAX`, and
/// [`RecordError::Arrow`] if the batch will not assemble.
pub fn build_batch(schema: &TableSchema, rows: &[(u64, Vec<u8>)]) -> Result<RecordBatch> {
    let parsed: Vec<serde_json::Map<String, serde_json::Value>> = rows
        .iter()
        .map(|(_, json)| {
            let v: serde_json::Value = serde_json::from_slice(json)?;
            match v {
                serde_json::Value::Object(m) => Ok(m),
                _ => Err(RecordError::malformed(
                    "record",
                    format!("a record on {} is not a JSON object", schema.channel),
                )),
            }
        })
        .collect::<Result<_>>()?;

    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(schema.columns.len());
    for col in &schema.columns {
        let array: ArrayRef = match col.kind {
            ColumnKind::Int => {
                let mut b = Int64Builder::with_capacity(rows.len());
                for (i, obj) in parsed.iter().enumerate() {
                    match obj.get(&col.name) {
                        None | Some(serde_json::Value::Null) => {
                            if col.name == TIME_COLUMN {
                                b.append_value(i64::try_from(rows[i].0).map_err(|_| {
                                    RecordError::Unrepresentable {
                                        what: "sim_time_ns",
                                        value: rows[i].0.to_string(),
                                        ty: "i64",
                                    }
                                })?);
                            } else {
                                b.append_null();
                            }
                        }
                        Some(v) => b.append_value(as_i64(&col.name, v)?),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnKind::Float => {
                let quantum = col.quantum.unwrap_or(crate::grid::Q_PROBABILITY);
                let mut b = Float64Builder::with_capacity(rows.len());
                for obj in &parsed {
                    match obj.get(&col.name) {
                        None | Some(serde_json::Value::Null) => b.append_null(),
                        Some(v) => {
                            let x = as_f64(&col.name, v)?;
                            b.append_value(v2xw_core::math::quantize_to(x, quantum));
                        }
                    }
                }
                Arc::new(b.finish())
            }
            ColumnKind::Text => {
                let mut b = StringBuilder::new();
                for obj in &parsed {
                    match obj.get(&col.name) {
                        None | Some(serde_json::Value::Null) => b.append_null(),
                        Some(serde_json::Value::String(s)) => b.append_value(s),
                        // A nested object or array is stored as its compact JSON, and
                        // every float inside it goes through the same declared grid a
                        // flat column's would (D9). Stringifying it unquantised was the
                        // hole `scan_batches` could not see, because it only reads float
                        // columns.
                        Some(other) => b.append_value(
                            super::schema::quantise_nested(&col.name, other).to_string(),
                        ),
                    }
                }
                Arc::new(b.finish())
            }
            ColumnKind::Bool => {
                let mut b = BooleanBuilder::with_capacity(rows.len());
                for obj in &parsed {
                    match obj.get(&col.name) {
                        None | Some(serde_json::Value::Null) => b.append_null(),
                        Some(serde_json::Value::Bool(v)) => b.append_value(*v),
                        Some(other) => {
                            return Err(RecordError::malformed(
                                "record",
                                format!(
                                    "{}.{} is {other}, not a boolean",
                                    schema.channel, col.name
                                ),
                            ));
                        }
                    }
                }
                Arc::new(b.finish())
            }
        };
        arrays.push(array);
    }
    Ok(RecordBatch::try_new(
        Arc::new(schema.arrow_schema()),
        arrays,
    )?)
}

fn as_i64(field: &str, v: &serde_json::Value) -> Result<i64> {
    if let Some(i) = v.as_i64() {
        return Ok(i);
    }
    if let Some(u) = v.as_u64() {
        return i64::try_from(u).map_err(|_| RecordError::Unrepresentable {
            what: "integer column",
            value: format!("{field} = {u}"),
            ty: "i64",
        });
    }
    if let Some(b) = v.as_bool() {
        return Ok(i64::from(b));
    }
    Err(RecordError::malformed(
        "record",
        format!("{field} = {v} is not an integer"),
    ))
}

fn as_f64(field: &str, v: &serde_json::Value) -> Result<f64> {
    v.as_f64()
        .ok_or_else(|| RecordError::malformed("record", format!("{field} = {v} is not a number")))
}
