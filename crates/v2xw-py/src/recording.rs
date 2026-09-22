//! `v2xw.Recording` — opening a recording, verifying it, and getting its records out.
//!
//! # Three ways out, for three different jobs
//!
//! * [`PyRecording::table`] / [`PyRecording::arrow`] — a whole channel as one Arrow table.
//!   `v2xw-record`'s exporter infers the column schema from the records themselves
//!   (`TableSchema::infer`) and builds the batch, so this is the columnar path and the one
//!   to reach for with a dataset. `arrow` hands the buffers to `pyarrow` through the C data
//!   interface; `table` returns the Arrow IPC stream as `bytes` for a caller without
//!   `pyarrow`.
//! * [`PyRecording::records`] — a list of decoded dictionaries. One Python object per
//!   field. Honest and slow, for looking at a handful of records.
//! * [`PyRecording::__iter__`] — the same, streamed, so a recording larger than memory can
//!   be walked without materialising it.
//!
//! # Verification is not free and is not implied
//!
//! Opening a recording reads its footer and index. It does **not** check the chunk CRCs,
//! because that means decompressing the whole file. [`PyRecording::verify`] does, and its
//! report distinguishes "checked and correct" from "there was nothing to check": a chunk
//! may declare `uncompressed_crc = 0`, which the container defines as *absent*, and a
//! reader that called such a file verified would be lying. `integrity_verified` is false
//! when any chunk was unchecked, and [`PyRecording::require_chunk_checksums`] turns that
//! into a refusal.

use std::collections::BTreeMap;

use arrow::pyarrow::IntoPyArrow;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use v2xw_record::export::{schema::TableSchema, table::build_batch};
use v2xw_record::{MemorySource, Reader};

use crate::err;
use crate::scenario::to_py_dict;

/// An open recording.
#[pyclass(name = "Recording", module = "v2xw", unsendable)]
pub struct PyRecording {
    reader: Reader<MemorySource>,
    path: String,
}

impl PyRecording {
    /// The `(sim_time, json)` rows of one channel, in the order the file stores them.
    fn rows_of(&mut self, channel: Option<&str>) -> PyResult<Vec<(u64, Vec<u8>)>> {
        let records = self.reader.records(channel).map_err(err::record)?;
        Ok(records.into_iter().map(|r| (r.sim_time, r.json)).collect())
    }
}

#[pymethods]
impl PyRecording {
    /// Opens a recording.
    #[staticmethod]
    fn open(path: &str) -> PyResult<Self> {
        let reader = Reader::open(path).map_err(err::record)?;
        Ok(Self {
            reader,
            path: path.to_string(),
        })
    }

    /// Opens a recording already in memory.
    #[staticmethod]
    fn open_bytes(data: &[u8]) -> PyResult<Self> {
        let reader = Reader::open_bytes(data.to_vec()).map_err(err::record)?;
        Ok(Self {
            reader,
            path: "<bytes>".to_string(),
        })
    }

    /// The file this was opened from.
    #[getter]
    fn path(&self) -> &str {
        &self.path
    }

    /// The run manifest the recording carries, as a `dict`.
    ///
    /// Empty if the file has no manifest, which is what an interrupted run looks like.
    #[getter]
    fn manifest<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.reader.manifest_json() {
            Some(json) => py.import("json")?.call_method1("loads", (json,)),
            None => Ok(PyDict::new(py).into_any()),
        }
    }

    /// The recording's metadata keys and values, verbatim.
    #[getter]
    fn metadata(&self) -> BTreeMap<String, String> {
        self.reader.manifest().clone()
    }

    /// Every channel in the file: name, visibility tag and message count.
    #[getter]
    fn channels<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(py);
        for ch in self.reader.channels() {
            // Built field by field rather than through serde: `ChannelMeta` is a reader-side
            // view and is not `Serialize`, and making it so is `v2xw-record`'s decision, not
            // this crate's.
            let row = PyDict::new(py);
            row.set_item("id", ch.id)?;
            row.set_item("topic", &ch.topic)?;
            row.set_item("message_encoding", &ch.message_encoding)?;
            row.set_item("visibility", ch.visibility())?;
            row.set_item("ground_truth", ch.is_ground_truth())?;
            row.set_item("metadata", ch.metadata.clone())?;
            out.append(row)?;
        }
        Ok(out)
    }

    /// The channel names that carry ground truth, which an exporter must keep separate.
    #[getter]
    fn ground_truth_channels(&self) -> Vec<String> {
        self.reader
            .ground_truth_channels()
            .map(|c| c.topic.clone())
            .collect()
    }

    /// Makes a chunk with no stored checksum a refusal rather than a caveat.
    fn require_chunk_checksums(&mut self, require: bool) {
        self.reader.require_chunk_checksums(require);
    }

    /// Walks the whole file, checking what can be checked, and reports what it found.
    fn verify<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let r = self.reader.verify().map_err(err::record)?;
        let d = PyDict::new(py);
        d.set_item("frames", r.frames)?;
        d.set_item("records", r.records)?;
        d.set_item("keyframes", r.keyframes)?;
        d.set_item("deltas", r.deltas)?;
        d.set_item("unknown_frames", r.unknown_frames)?;
        d.set_item("provenance_ids", r.provenance_ids)?;
        d.set_item("provenance_references", r.provenance_references)?;
        d.set_item("span_ns", r.span)?;
        d.set_item("max_snapshot_gap_ns", r.max_snapshot_gap_ns)?;
        d.set_item("chunks_checksummed", r.chunks_checksummed)?;
        d.set_item("chunks_without_checksum", r.chunks_without_checksum)?;
        // The distinction the module note is about: `True` only when every chunk carried a
        // checksum and every checksum matched. A file whose chunks declare no CRC reads
        // fine and verifies to `False`, because nothing in it was actually checked.
        d.set_item("integrity_verified", r.integrity_verified())?;
        Ok(d)
    }

    /// SHA-256 over the recording's content, excluding the container's framing.
    fn content_digest(&mut self) -> PyResult<String> {
        let d = self.reader.content_digest().map_err(err::record)?;
        Ok(d.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The records of one channel (or all of them) as a list of dictionaries.
    ///
    /// **The slow path**, by construction: one Python object per field. Use
    /// [`PyRecording::arrow`] for a table.
    #[pyo3(signature = (channel=None, limit=None))]
    fn records<'py>(
        &mut self,
        py: Python<'py>,
        channel: Option<&str>,
        limit: Option<usize>,
    ) -> PyResult<Bound<'py, PyList>> {
        let records = self.reader.records(channel).map_err(err::record)?;
        let out = PyList::empty(py);
        let json = py.import("json")?;
        for r in records.iter().take(limit.unwrap_or(usize::MAX)) {
            let text = std::str::from_utf8(&r.json).map_err(|e| {
                err::RecordingError::new_err(format!("record on {}: {e}", r.channel))
            })?;
            let value = json.call_method1("loads", (text,))?;
            let row = PyDict::new(py);
            row.set_item("channel", &r.channel)?;
            row.set_item("sim_time_ns", r.sim_time)?;
            row.set_item("record", value)?;
            out.append(row)?;
        }
        Ok(out)
    }

    /// One channel as a `pyarrow.RecordBatch`, sharing the buffers.
    ///
    /// The column schema is inferred from the records themselves, and every float column
    /// carries the grid its writer quantised it onto (build decision D9) in the field
    /// metadata. `ground_truth=False` drops the ground-truth columns, which is what an
    /// export that must not leak oracle state does.
    #[pyo3(signature = (channel, ground_truth=true))]
    fn arrow(&mut self, py: Python<'_>, channel: &str, ground_truth: bool) -> PyResult<PyObject> {
        let rows = self.rows_of(Some(channel))?;
        let schema = TableSchema::infer(channel, &rows).map_err(err::record)?;
        let schema = if ground_truth {
            schema
        } else {
            schema.without_ground_truth()
        };
        let batch = build_batch(&schema, &rows).map_err(err::record)?;
        batch.into_pyarrow(py)
    }

    /// One channel as an Arrow IPC stream, as `bytes` — the no-`pyarrow` form of
    /// [`PyRecording::arrow`].
    #[pyo3(signature = (channel, ground_truth=true))]
    fn table<'py>(
        &mut self,
        py: Python<'py>,
        channel: &str,
        ground_truth: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let rows = self.rows_of(Some(channel))?;
        let schema = TableSchema::infer(channel, &rows).map_err(err::record)?;
        let schema = if ground_truth {
            schema
        } else {
            schema.without_ground_truth()
        };
        let batch = build_batch(&schema, &rows).map_err(err::record)?;
        let bytes = v2xw_metrics::arrow_out::to_ipc(&[batch]).map_err(err::metric)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// The inferred column schema of one channel, as a `dict`.
    fn table_schema<'py>(&mut self, py: Python<'py>, channel: &str) -> PyResult<Bound<'py, PyAny>> {
        let rows = self.rows_of(Some(channel))?;
        let schema = TableSchema::infer(channel, &rows).map_err(err::record)?;
        to_py_dict(py, "table schema", &schema)
    }

    /// How many VWP snapshot frames the recording holds.
    fn frame_count(&mut self) -> PyResult<usize> {
        Ok(self.reader.replay().map_err(err::record)?.len())
    }

    fn __repr__(&self) -> String {
        format!("<v2xw.Recording path={:?}>", self.path)
    }
}
