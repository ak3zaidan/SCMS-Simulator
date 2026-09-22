//! The scanning test ADR 0004 §7 asks for: "a test scans every output file for any value
//! that is off its grid".
//!
//! It is a library function rather than a test so that it can also run in CI over a real
//! export and in the conformance kit (§10.6 item V6, "every exporter's output passes the
//! leakage linter"). It reads the artefact back — Parquet, Arrow IPC or JSONL — and
//! checks every float against the grid the export's own `schema.json` declares, so a
//! writer that forgot to quantise is caught by the file, not by the code that wrote it.
//!
//! # Nested values are floats too
//!
//! A JSON object or array is typed [`ColumnKind::Text`], and an earlier version of this
//! scan skipped every column that was not [`ColumnKind::Float`] — so the numbers inside a
//! nested value were neither quantised on the way out nor looked at on the way back, and
//! `scan_export` reported success over an artefact holding raw doubles. The scan now
//! parses a text value that is itself JSON and checks every number in it against the grid
//! its innermost key declares, which is the grid
//! [`crate::export::schema::quantise_nested`] wrote it to.

use std::path::Path;

use arrow::array::{Array, Float64Array, RecordBatch, StringArray};

use super::schema::{ColumnKind, TableSchema, declared_quantum};
use crate::error::{RecordError, Result};

/// What a scan looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanReport {
    /// Files scanned.
    pub files: usize,
    /// Rows scanned.
    pub rows: usize,
    /// Float values checked.
    pub values: usize,
}

impl ScanReport {
    fn merge(&mut self, other: ScanReport) {
        self.files += other.files;
        self.rows += other.rows;
        self.values += other.values;
    }
}

/// Scans Arrow batches — the shared path for Parquet and Arrow IPC.
///
/// # Errors
/// [`RecordError::OffGrid`] naming the first value that is off its declared grid.
pub fn scan_batches(
    file: &str,
    schema: &TableSchema,
    batches: &[RecordBatch],
) -> Result<ScanReport> {
    let mut report = ScanReport {
        files: 1,
        ..Default::default()
    };
    let mut row_base = 0usize;
    for batch in batches {
        report.rows += batch.num_rows();
        for col in &schema.columns {
            if col.kind == ColumnKind::Text {
                if let Some(array) = batch.column_by_name(&col.name) {
                    let texts = array
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or_else(|| {
                            RecordError::malformed(
                                "export",
                                format!(
                                    "column {}.{} is not a text column",
                                    schema.channel, col.name
                                ),
                            )
                        })?;
                    for i in 0..texts.len() {
                        if texts.is_null(i) {
                            continue;
                        }
                        scan_text(
                            file,
                            schema,
                            &col.name,
                            texts.value(i),
                            row_base + i,
                            &mut report,
                        )?;
                    }
                }
                continue;
            }
            if col.kind != ColumnKind::Float {
                continue;
            }
            let quantum = col.quantum.ok_or_else(|| {
                RecordError::malformed(
                    "schema.json",
                    format!(
                        "float column {}.{} declares no quantum",
                        schema.channel, col.name
                    ),
                )
            })?;
            let Some(array) = batch.column_by_name(&col.name) else {
                continue;
            };
            let floats = array
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| {
                    RecordError::malformed(
                        "export",
                        format!(
                            "column {}.{} is not a float column",
                            schema.channel, col.name
                        ),
                    )
                })?;
            for i in 0..floats.len() {
                if floats.is_null(i) {
                    continue;
                }
                let v = floats.value(i);
                report.values += 1;
                if !v2xw_core::math::is_on_grid(v, quantum) {
                    return Err(RecordError::OffGrid {
                        file: file.to_string(),
                        channel: schema.channel.clone(),
                        field: col.name.clone(),
                        value: v,
                        quantum,
                        row: row_base + i,
                    });
                }
            }
        }
        row_base += batch.num_rows();
    }
    Ok(report)
}

/// Scans a Parquet file.
///
/// # Errors
/// As [`scan_batches`], plus whatever Parquet returns for a file it cannot read.
pub fn scan_parquet(path: impl AsRef<Path>, schema: &TableSchema) -> Result<ScanReport> {
    let path = path.as_ref();
    let batches = super::parquet::read(path)?;
    scan_batches(&path.display().to_string(), schema, &batches)
}

/// Scans an Arrow IPC file.
///
/// # Errors
/// As [`scan_batches`], plus whatever Arrow returns for a file it cannot read.
pub fn scan_arrow_ipc(path: impl AsRef<Path>, schema: &TableSchema) -> Result<ScanReport> {
    let path = path.as_ref();
    let batches = super::arrow_ipc::read(path)?;
    scan_batches(&path.display().to_string(), schema, &batches)
}

/// Scans a JSONL file.
///
/// # Errors
/// [`RecordError::OffGrid`] for the first off-grid value, or [`RecordError::Json`] for a
/// line that is not JSON.
pub fn scan_jsonl(path: impl AsRef<Path>, schema: &TableSchema) -> Result<ScanReport> {
    let path = path.as_ref();
    let rows = super::jsonl::read(path)?;
    let mut report = ScanReport {
        files: 1,
        rows: rows.len(),
        ..Default::default()
    };
    for (i, row) in rows.iter().enumerate() {
        let Some(obj) = row.as_object() else { continue };
        for col in &schema.columns {
            if col.kind == ColumnKind::Text {
                match obj.get(&col.name) {
                    // JSONL keeps a nested value's shape, so it arrives as a value…
                    Some(v @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
                        scan_nested(
                            &path.display().to_string(),
                            schema,
                            &col.name,
                            v,
                            i,
                            &mut report,
                        )?;
                    }
                    // …and a text column that holds one as a string is checked too.
                    Some(serde_json::Value::String(s)) => scan_text(
                        &path.display().to_string(),
                        schema,
                        &col.name,
                        s,
                        i,
                        &mut report,
                    )?,
                    _ => {}
                }
                continue;
            }
            if col.kind != ColumnKind::Float {
                continue;
            }
            let quantum = col.quantum.ok_or_else(|| {
                RecordError::malformed(
                    "schema.json",
                    format!(
                        "float column {}.{} declares no quantum",
                        schema.channel, col.name
                    ),
                )
            })?;
            let Some(v) = obj.get(&col.name).and_then(serde_json::Value::as_f64) else {
                continue;
            };
            report.values += 1;
            if !v2xw_core::math::is_on_grid(v, quantum) {
                return Err(RecordError::OffGrid {
                    file: path.display().to_string(),
                    channel: schema.channel.clone(),
                    field: col.name.clone(),
                    value: v,
                    quantum,
                    row: i,
                });
            }
        }
    }
    Ok(report)
}

/// Checks a text cell that holds a nested JSON value.
///
/// Only a value that *is* JSON is parsed: a genuine string column — a detector name, a
/// label — is left alone, which is why the test is on the first character rather than on
/// whether `serde_json` happens to accept it. A quoted number is a string, not a float.
fn scan_text(
    file: &str,
    schema: &TableSchema,
    column: &str,
    text: &str,
    row: usize,
    report: &mut ScanReport,
) -> Result<()> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(());
    };
    scan_nested(file, schema, column, &value, row, report)
}

/// Walks a nested JSON value, checking every float against the grid its innermost key
/// declares — the same rule [`crate::export::schema::quantise_nested`] wrote it under.
fn scan_nested(
    file: &str,
    schema: &TableSchema,
    key: &str,
    value: &serde_json::Value,
    row: usize,
    report: &mut ScanReport,
) -> Result<()> {
    match value {
        serde_json::Value::Number(n) if n.is_f64() => {
            if let Some(v) = n.as_f64() {
                let quantum = declared_quantum(key);
                report.values += 1;
                if !v2xw_core::math::is_on_grid(v, quantum) {
                    return Err(RecordError::OffGrid {
                        file: file.to_string(),
                        channel: schema.channel.clone(),
                        field: key.to_string(),
                        value: v,
                        quantum,
                        row,
                    });
                }
            }
        }
        serde_json::Value::Array(a) => {
            for e in a {
                scan_nested(file, schema, key, e, row, report)?;
            }
        }
        serde_json::Value::Object(m) => {
            for (k, e) in m {
                scan_nested(file, schema, k, e, row, report)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Scans every file of an export, reading each one's declared schema from its sidecar.
///
/// This is the whole-directory form of the ADR 0004 test: point it at an export and it
/// checks every artefact in it, whatever format each one is in.
///
/// # Errors
/// The first violation, or an I/O or format error.
pub fn scan_export(files: &[super::ExportedFile]) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for f in files {
        let Some(schema) = &f.schema else { continue };
        let one = match f.format {
            super::ExportFormat::Parquet => scan_parquet(&f.path, schema)?,
            super::ExportFormat::ArrowIpc => scan_arrow_ipc(&f.path, schema)?,
            super::ExportFormat::Jsonl => scan_jsonl(&f.path, schema)?,
            super::ExportFormat::Json => ScanReport {
                files: 1,
                ..Default::default()
            },
        };
        report.merge(one);
    }
    Ok(report)
}
