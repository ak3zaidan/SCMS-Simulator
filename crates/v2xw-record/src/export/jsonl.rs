//! JSONL export — "retained for the legacy MA dataset profile" (ADR 0008 decision 2,
//! 08-measurement-and-data.md §5).
//!
//! Two things make this more than `println!("{json}")`. Every float goes through its
//! column's declared grid first — including the ones nested inside an object or an array,
//! which used to be copied through raw — because the legacy engine's one unrounded field
//! is the whole reason D9 exists. And every object's keys are written in sorted order through
//! [`v2xw_core::canonical_json`], so the file is reproducible: the legacy corpus's
//! `st_bbox` broke a digest by one ULP, and an insertion-ordered key set would break one
//! by nothing at all.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::schema::{ColumnKind, TIME_COLUMN, TableSchema};
use crate::error::{RecordError, Result};

/// Writes a channel's rows as JSON Lines.
///
/// Columns the schema does not declare are dropped, so the `NODE-only` projection works
/// here exactly as it does for Parquet.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be written, [`RecordError::Json`] if a record
/// is not a JSON object.
pub fn write(path: impl AsRef<Path>, schema: &TableSchema, rows: &[(u64, Vec<u8>)]) -> Result<u64> {
    let path = path.as_ref();
    let file = File::create(path).map_err(|e| RecordError::io(path, e))?;
    let mut out = BufWriter::new(file);
    for (at, json) in rows {
        let value: serde_json::Value = serde_json::from_slice(json)?;
        let obj = value.as_object().ok_or_else(|| {
            RecordError::malformed(
                "record",
                format!("a record on {} is not a JSON object", schema.channel),
            )
        })?;
        let mut row = serde_json::Map::new();
        for col in &schema.columns {
            let v = match obj.get(&col.name) {
                Some(serde_json::Value::Null) | None if col.name == TIME_COLUMN => {
                    serde_json::Value::from(*at)
                }
                None => continue,
                Some(serde_json::Value::Null) => serde_json::Value::Null,
                Some(v) => match col.kind {
                    ColumnKind::Float => {
                        let x = v.as_f64().ok_or_else(|| {
                            RecordError::malformed(
                                "record",
                                format!("{}.{} = {v} is not a number", schema.channel, col.name),
                            )
                        })?;
                        let q = v2xw_core::math::quantize_to(
                            x,
                            col.quantum.unwrap_or(crate::grid::Q_PROBABILITY),
                        );
                        serde_json::Number::from_f64(q)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null)
                    }
                    // A nested object or array keeps its shape in JSONL, so its floats
                    // are quantised in place by their innermost key rather than copied
                    // through — the same gate the flat float columns go through (D9).
                    _ => super::schema::quantise_nested(&col.name, v),
                },
            };
            row.insert(col.name.clone(), v);
        }
        let line = v2xw_core::canonical_json(&serde_json::Value::Object(row))
            .map_err(|e| RecordError::malformed("jsonl row", e.to_string()))?;
        out.write_all(&line).map_err(|e| RecordError::io(path, e))?;
        out.write_all(b"\n").map_err(|e| RecordError::io(path, e))?;
    }
    out.flush().map_err(|e| RecordError::io(path, e))?;
    Ok(std::fs::metadata(path)
        .map_err(|e| RecordError::io(path, e))?
        .len())
}

/// Reads a JSONL file back as one JSON value per line.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be read, [`RecordError::Json`] if a line is not
/// JSON.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<serde_json::Value>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| RecordError::io(path, e))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}
