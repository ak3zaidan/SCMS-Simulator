//! The scenario's `exporters` list: what a finished run writes beside its recording.
//!
//! 03-interfaces.md §13 lets a scenario name its exporters, and until this module the list
//! was refused outright: the engine had no exporter stage, and a list nobody read was worse
//! than none. The stage is deliberately *after* the run and *over the recording*, not inside
//! the run loop: every exporter reads the same stored records a reviewer would read, so an
//! export can never disagree with the recording beside it, and the run loop stays free of
//! file formats.
//!
//! | id | What it writes |
//! |---|---|
//! | `recording` | keeps the MCAP recording itself (it is written whenever an exporter is named) |
//! | `jsonl` | one JSON Lines table per recorded channel, with a schema sidecar |
//! | `parquet` | the same tables as Apache Parquet — the primary analysis format (08-measurement-and-data.md §5) |
//! | `arrow` | the same tables as Arrow IPC |
//!
//! `opts.profile` is `full` (the default) or `node`: the `node` profile drops every
//! ground-truth channel and column (§5), which is what a blind evaluation hands a detector.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{EngineError, Result};
use crate::scenario::schema::ExporterSpec;

/// The exporter ids this build implements.
pub const EXPORTERS: [&str; 4] = ["recording", "jsonl", "parquet", "arrow"];

/// What one exporter wrote.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Exported {
    /// The exporter id.
    pub exporter: String,
    /// Every file it wrote, with its row count and size.
    pub files: Vec<ExportedPath>,
}

/// One written file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportedPath {
    /// Where it is.
    pub path: PathBuf,
    /// Rows, for a table; 0 for a sidecar or the recording.
    pub rows: usize,
    /// Bytes on disk.
    pub bytes: u64,
}

/// The profile an exporter's `opts` asks for.
///
/// # Errors
/// A conflict naming the field when `opts.profile` is neither `full` nor `node`.
pub fn profile_of(spec: &ExporterSpec, index: usize) -> Result<v2xw_record::ExportProfile> {
    match spec.opts.get("profile").and_then(Value::as_str) {
        None | Some("full") => Ok(v2xw_record::ExportProfile::Full),
        Some("node") => Ok(v2xw_record::ExportProfile::NodeOnly),
        Some(other) => Err(EngineError::Scenario(crate::ScenarioError::conflict(
            format!("exporters[{index}].opts.profile"),
            format!("is '{other}'; an export profile is 'full' or 'node'"),
        ))),
    }
}

/// Runs every exporter in `specs` over the finished recording at `recording`, writing into
/// `out_dir`. Exporters run in list order and each writes into `out_dir/<id>/`.
///
/// # Errors
/// [`EngineError::Io`] if the recording cannot be read or a file cannot be written, and a
/// scenario conflict for an id this build does not implement.
pub fn run_exporters(specs: &[ExporterSpec], recording: &Path, out_dir: &Path) -> Result<Vec<Exported>> {
    let mut out = Vec::new();
    let mut records = None;
    for (i, spec) in specs.iter().enumerate() {
        let format = match spec.id.as_str() {
            "recording" => {
                let bytes = std::fs::metadata(recording).map(|m| m.len()).unwrap_or(0);
                out.push(Exported {
                    exporter: spec.id.clone(),
                    files: vec![ExportedPath { path: recording.to_path_buf(), rows: 0, bytes }],
                });
                continue;
            }
            "jsonl" => v2xw_record::ExportFormat::Jsonl,
            "parquet" => v2xw_record::ExportFormat::Parquet,
            "arrow" => v2xw_record::ExportFormat::ArrowIpc,
            other => {
                return Err(EngineError::Scenario(crate::ScenarioError::conflict(
                    format!("exporters[{i}].id"),
                    format!("'{other}' is not an exporter this build has; one of {}", EXPORTERS.join(", ")),
                )));
            }
        };
        if records.is_none() {
            let mut reader = v2xw_record::Reader::open(recording).map_err(record_error(recording))?;
            records = Some(reader.records(None).map_err(record_error(recording))?);
        }
        let dir = out_dir.join(&spec.id);
        std::fs::create_dir_all(&dir).map_err(|e| EngineError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let exporter = v2xw_record::Exporter::new(&dir, profile_of(spec, i)?).map_err(record_error(&dir))?;
        let files = exporter
            .export_all(records.as_deref().unwrap_or(&[]), format)
            .map_err(record_error(&dir))?;
        out.push(Exported {
            exporter: spec.id.clone(),
            files: files
                .into_iter()
                .map(|f| ExportedPath { path: f.path, rows: f.rows, bytes: f.bytes })
                .collect(),
        });
    }
    Ok(out)
}

fn record_error(path: &Path) -> impl Fn(v2xw_record::RecordError) -> EngineError + '_ {
    move |e| EngineError::Io {
        path: path.display().to_string(),
        source: std::io::Error::other(e.to_string()),
    }
}
