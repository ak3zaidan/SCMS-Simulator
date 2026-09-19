//! Parquet export — 08-measurement-and-data.md §5's primary format.

use std::fs::File;
use std::path::Path;

use arrow::array::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::error::{RecordError, Result};

/// Writes one batch as a Parquet file, zstd-compressed.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be created, [`RecordError::Arrow`] if Parquet
/// rejects the schema or the batch.
pub fn write(path: impl AsRef<Path>, batch: &RecordBatch) -> Result<u64> {
    let path = path.as_ref();
    let file = File::create(path).map_err(|e| RecordError::io(path, e))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).map_err(RecordError::from)?,
        ))
        .build();
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))?;
    writer.write(batch)?;
    writer.close()?;
    Ok(std::fs::metadata(path)
        .map_err(|e| RecordError::io(path, e))?
        .len())
}

/// Reads every batch back — used by the grid scan and by the round-trip tests.
///
/// # Errors
/// [`RecordError::Io`] if the file cannot be opened, [`RecordError::Arrow`] if Parquet
/// rejects it.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<RecordBatch>> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|e| RecordError::io(path, e))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut out = Vec::new();
    for batch in reader {
        out.push(batch?);
    }
    Ok(out)
}
