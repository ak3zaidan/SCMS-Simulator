//! Exporters — 08-measurement-and-data.md §5 and ADR 0008 decision 2.
//!
//! | Format | Used for | Module |
//! |---|---|---|
//! | Parquet | every tabular record channel; "the primary format" | [`parquet`] |
//! | Arrow IPC | metric batches and Python plug-in exchange | [`arrow_ipc`] |
//! | JSONL | the legacy MA dataset profile | [`jsonl`] |
//!
//! Every one of them writes through [`table::build_batch`] or [`jsonl::write`], and both
//! of those quantise each float to the grid its column declares (D9). The declared grids
//! are written next to the data as `schema.json`, and [`scan`] reads them back and checks
//! every value — the scanning test ADR 0004 §7 asks for.
//!
//! # Visibility
//!
//! [`ExportProfile::NodeOnly`] drops the ground-truth channels entirely and projects the
//! ground-truth *columns* out of the mixed ones (§5.2), so a `NODE` export and a `GT`
//! export never share a file (conformance V6). Each file's schema records which it is.

pub mod arrow_ipc;
pub mod jsonl;
pub mod parquet;
pub mod scan;
pub mod schema;
pub mod table;

use std::path::{Path, PathBuf};

use crate::channels;
use crate::error::{RecordError, Result};
use crate::reader::RecordedRecord;

pub use schema::{ColumnKind, ColumnSpec, TableSchema};

/// Which artefact format a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Apache Parquet.
    Parquet,
    /// Arrow IPC file format.
    ArrowIpc,
    /// JSON Lines.
    Jsonl,
    /// A plain JSON document — a schema sidecar or a manifest.
    Json,
}

impl ExportFormat {
    /// The file extension this format uses.
    pub const fn extension(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "parquet",
            ExportFormat::ArrowIpc => "arrow",
            ExportFormat::Jsonl => "jsonl",
            ExportFormat::Json => "json",
        }
    }
}

/// How much of the data an export may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportProfile {
    /// Everything the recording holds.
    #[default]
    Full,
    /// No ground-truth channel and no ground-truth column (§5.2).
    NodeOnly,
}

/// One file an export produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedFile {
    /// Where it is.
    pub path: PathBuf,
    /// What it is in.
    pub format: ExportFormat,
    /// The channel it holds, or `None` for a sidecar.
    pub channel: Option<String>,
    /// Rows written.
    pub rows: usize,
    /// Bytes written.
    pub bytes: u64,
    /// The schema its values are declared against, or `None` for a sidecar.
    pub schema: Option<TableSchema>,
}

/// Exports a recording's serde records to a directory.
#[derive(Debug, Clone)]
pub struct Exporter {
    dir: PathBuf,
    profile: ExportProfile,
}

impl Exporter {
    /// An exporter writing into `dir`, which is created if it does not exist.
    ///
    /// # Errors
    /// [`RecordError::Io`] if the directory cannot be created.
    pub fn new(dir: impl AsRef<Path>, profile: ExportProfile) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| RecordError::io(&dir, e))?;
        Ok(Exporter { dir, profile })
    }

    /// The directory being written into.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The profile.
    pub fn profile(&self) -> ExportProfile {
        self.profile
    }

    /// Exports one channel in one format, plus its `schema.json` sidecar.
    ///
    /// Returns the data file and the sidecar, in that order.
    ///
    /// # Errors
    /// [`RecordError::UnknownChannel`] for a channel outside 03-interfaces §14,
    /// [`RecordError::VisibilityDenied`] if the profile forbids the channel, and whatever
    /// the format's writer returns.
    pub fn export_channel(
        &self,
        channel: &str,
        records: &[RecordedRecord],
        format: ExportFormat,
    ) -> Result<Vec<ExportedFile>> {
        let spec = channels::by_name(channel)
            .ok_or_else(|| RecordError::UnknownChannel(channel.to_string()))?;
        if self.profile == ExportProfile::NodeOnly && spec.is_ground_truth_channel() {
            return Err(RecordError::VisibilityDenied {
                channel: channel.to_string(),
                visibility: spec.visibility,
            });
        }
        let rows: Vec<(u64, Vec<u8>)> = records
            .iter()
            .filter(|r| r.channel == channel)
            .map(|r| (r.sim_time, r.json.clone()))
            .collect();
        let inferred = TableSchema::infer(channel, &rows)?;
        let schema = if self.profile == ExportProfile::NodeOnly {
            inferred.without_ground_truth()
        } else {
            inferred
        };

        let stem = channel.replace('.', "_");
        let data_path = self.dir.join(format!("{stem}.{}", format.extension()));
        let bytes = match format {
            ExportFormat::Parquet => {
                let batch = table::build_batch(&schema, &rows)?;
                parquet::write(&data_path, &batch)?
            }
            ExportFormat::ArrowIpc => {
                let batch = table::build_batch(&schema, &rows)?;
                arrow_ipc::write(&data_path, &[batch])?
            }
            ExportFormat::Jsonl => jsonl::write(&data_path, &schema, &rows)?,
            ExportFormat::Json => {
                return Err(RecordError::malformed(
                    "export",
                    "plain JSON is for sidecars, not for a record table",
                ));
            }
        };
        let schema_path = self.dir.join(format!("{stem}.schema.json"));
        let schema_bytes = schema.to_json()?;
        std::fs::write(&schema_path, &schema_bytes)
            .map_err(|e| RecordError::io(&schema_path, e))?;
        Ok(vec![
            ExportedFile {
                path: data_path,
                format,
                channel: Some(channel.to_string()),
                rows: rows.len(),
                bytes,
                schema: Some(schema),
            },
            ExportedFile {
                path: schema_path,
                format: ExportFormat::Json,
                channel: Some(channel.to_string()),
                rows: 0,
                bytes: schema_bytes.len() as u64,
                schema: None,
            },
        ])
    }

    /// Exports every channel present in `records`, each in `format`.
    ///
    /// Channels are visited in name order, so an export is reproducible.
    ///
    /// # Errors
    /// As [`Exporter::export_channel`], except that a channel the profile forbids is
    /// skipped rather than refused — a `NODE-only` export of a full recording is a normal
    /// thing to want.
    pub fn export_all(
        &self,
        records: &[RecordedRecord],
        format: ExportFormat,
    ) -> Result<Vec<ExportedFile>> {
        let mut names: Vec<&str> = records.iter().map(|r| r.channel.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let mut out = Vec::new();
        for name in names {
            let forbidden = self.profile == ExportProfile::NodeOnly
                && channels::by_name(name).is_some_and(|s| s.is_ground_truth_channel());
            if forbidden {
                continue;
            }
            out.extend(self.export_channel(name, records, format)?);
        }
        Ok(out)
    }
}
