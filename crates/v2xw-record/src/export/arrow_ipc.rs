//! Arrow IPC export — ADR 0008 decision 2's format for "tabular metric batches and for
//! Python plug-in exchange".
//!
//! The IPC *file* format rather than the stream format: it carries a footer, so a reader
//! can take a single batch out of a large export without walking the whole thing, which
//! is what a notebook plotting one metric wants.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use arrow::array::RecordBatch;
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;

use crate::error::{RecordError, Result};

/// Writes batches as one Arrow IPC file.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be created, [`RecordError::Arrow`] if Arrow
/// rejects a batch.
pub fn write(path: impl AsRef<Path>, batches: &[RecordBatch]) -> Result<u64> {
    let path = path.as_ref();
    let schema = match batches.first() {
        Some(b) => b.schema(),
        None => {
            return Err(RecordError::malformed(
                "arrow ipc export",
                "there is nothing to write: an IPC file needs at least one batch for its schema",
            ));
        }
    };
    let file = File::create(path).map_err(|e| RecordError::io(path, e))?;
    let mut writer = FileWriter::try_new(BufWriter::new(file), schema.as_ref())?;
    for batch in batches {
        writer.write(batch)?;
    }
    writer.finish()?;
    Ok(std::fs::metadata(path)
        .map_err(|e| RecordError::io(path, e))?
        .len())
}

/// Reads an Arrow IPC file back.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be opened, [`RecordError::Arrow`] if Arrow
/// rejects it.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<RecordBatch>> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|e| RecordError::io(path, e))?;
    let reader = FileReader::try_new(file, None)?;
    let mut out = Vec::new();
    for batch in reader {
        out.push(batch?);
    }
    Ok(out)
}
