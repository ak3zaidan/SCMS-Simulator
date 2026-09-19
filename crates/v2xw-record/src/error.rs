//! The crate's error type.
//!
//! Every fallible entry point in this crate returns [`RecordError`]. Nothing here
//! panics on malformed input: a truncated file, a frame whose `body_len` lies, a chunk
//! index pointing past the end of the file and a zstd frame that will not decompress all
//! come back as a named variant. That is a requirement, not a courtesy — a recording is
//! the artefact a reviewer reaches for *after* something went wrong, so it is routinely
//! read half-written.

use std::path::PathBuf;

use v2xw_core::time::SimTime;

/// The result type of every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, RecordError>;

/// Everything that can go wrong writing, reading, seeking or exporting a recording.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecordError {
    /// A structure ended before the field being read.
    ///
    /// Carries what was being read so the message names the defect rather than an offset
    /// in the abstract: `truncated vwp Delta.moved: need 20 bytes at 64, have 12`.
    #[error("truncated {what}: need {need} bytes at offset {at}, have {have}")]
    Truncated {
        /// The structure being decoded, e.g. `"vwp Keyframe.actors"`.
        what: &'static str,
        /// The byte offset the read started at.
        at: usize,
        /// How many bytes the field needs.
        need: usize,
        /// How many bytes remain.
        have: usize,
    },

    /// A fixed field holds a value the specification does not allow.
    #[error("malformed {what}: {detail}")]
    Malformed {
        /// The structure being decoded.
        what: &'static str,
        /// What is wrong with it.
        detail: String,
    },

    /// The frame's magic is not `0x31505756` (§2.1; conformance F1).
    #[error("bad frame magic {found:#010x}, expected {expected:#010x}")]
    BadMagic {
        /// The magic that was read.
        found: u32,
        /// The magic the specification fixes.
        expected: u32,
    },

    /// The frame or the recording declares a protocol major version this build cannot
    /// parse (§8.6, JSON-RPC error `-32050`).
    #[error("unsupported VWP major version {found}, this build speaks {supported}")]
    UnsupportedVersion {
        /// The version on the wire.
        found: u16,
        /// The version this build implements.
        supported: u16,
    },

    /// A value does not fit the wire type its field declares, and no escape applies.
    #[error("{what} = {value} does not fit {ty}")]
    Unrepresentable {
        /// The field.
        what: &'static str,
        /// The offending value, already formatted.
        value: String,
        /// The wire type it has to fit.
        ty: &'static str,
    },

    /// The recording is internally inconsistent — [`crate::reader::Reader::verify`]'s
    /// findings, and anything the reader trips over while replaying.
    #[error("inconsistent recording at sim time {at} ({frames} frames in): {detail}")]
    Inconsistent {
        /// The simulated time of the frame that failed the check.
        at: SimTime,
        /// How many frames had been read.
        frames: u64,
        /// What the check found.
        detail: String,
    },

    /// The recording has no summary section, so it cannot be indexed or seeked.
    ///
    /// This is what a recording whose writer was killed before `finish()` looks like.
    #[error("recording has no summary section: it was not finished, so it cannot be seeked")]
    NoSummary,

    /// A seek target lies outside the recorded interval (JSON-RPC error `-32003`).
    #[error("seek target {target} ns is outside the recording [{min}, {max}]")]
    SeekOutOfRange {
        /// The requested time.
        target: SimTime,
        /// The first recorded snapshot time.
        min: SimTime,
        /// The last recorded snapshot time.
        max: SimTime,
    },

    /// A channel was used that the recording does not declare.
    #[error("unknown channel {0:?}")]
    UnknownChannel(String),

    /// A record was written to a channel whose visibility forbids it
    /// ([`v2xw_core::Visibility::allowed_on_node_channel`]).
    #[error("record with visibility {visibility} may not be written to node channel {channel:?}")]
    VisibilityDenied {
        /// The channel written to.
        channel: String,
        /// The record's tag.
        visibility: v2xw_core::Visibility,
    },

    /// An exported value is not on its field's declared grid (ADR 0004 §7, D9).
    #[error("{file}: {channel}.{field} = {value} is off its {quantum} grid at row {row}")]
    OffGrid {
        /// The file the scan found it in.
        file: String,
        /// The channel the row belongs to.
        channel: String,
        /// The field name.
        field: String,
        /// The offending value.
        value: f64,
        /// The grid the field declares.
        quantum: f64,
        /// The row index within the file.
        row: usize,
    },

    /// The MCAP container rejected an operation.
    #[error("mcap: {0}")]
    Mcap(#[from] mcap::McapError),

    /// Arrow or Parquet rejected an operation.
    #[error("arrow/parquet: {0}")]
    Arrow(String),

    /// A JSON record would not encode or decode.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// The file system refused.
    #[error("io {path:?}: {source}")]
    Io {
        /// The path being read or written, when one is known.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl RecordError {
    /// An [`RecordError::Io`] carrying the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        RecordError::Io {
            path: path.into(),
            source,
        }
    }

    /// A [`RecordError::Malformed`] from a formatted detail.
    pub fn malformed(what: &'static str, detail: impl Into<String>) -> Self {
        RecordError::Malformed {
            what,
            detail: detail.into(),
        }
    }
}

impl From<arrow::error::ArrowError> for RecordError {
    fn from(e: arrow::error::ArrowError) -> Self {
        RecordError::Arrow(e.to_string())
    }
}

impl From<parquet::errors::ParquetError> for RecordError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        RecordError::Arrow(e.to_string())
    }
}
