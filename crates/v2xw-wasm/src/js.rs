//! The JavaScript surface: one class, and pointers into linear memory.
//!
//! Compiled only for `wasm32`, because `wasm-bindgen` has no meaning anywhere else. The
//! logic is all in [`crate::session`]; nothing here decides anything, so there is nothing
//! here the host-side tests cannot reach.
//!
//! # Driving it
//!
//! Every call that may need bytes returns a `Float64Array` of `[offset, len, …]`. Empty
//! means it finished; non-empty means fetch those ranges, hand them back with `supply`
//! and call again:
//!
//! ```js
//! const r = ReplayReader.ranged(Number(size));
//! for (let ranges = r.prepare(); ranges.length; ranges = r.prepare()) {
//!   await fetchRanges(url, ranges, r);
//! }
//! for (let ranges = r.seek(12.5); ranges.length; ranges = r.seek(12.5)) {
//!   await fetchRanges(url, ranges, r);
//! }
//! ```
//!
//! A recording opened with `fromBytes` never returns a non-empty array, so the same loop
//! serves both.
//!
//! # Why offsets are `f64` and times are `BigInt`
//!
//! A range offset is a file position: it is compared, added and put in a header, and an
//! `f64` is exact to 9 petabytes, so `Number` is the ergonomic choice and loses nothing.
//! A simulated time is an identity — it indexes keyframes and is compared for equality
//! against what the native reader reports — so it crosses as a `BigInt` and cannot be
//! rounded by accident. `seek` also takes seconds as a `Number`, which is what a scrub
//! bar has.
//!
//! # Zero copy
//!
//! The `*_ptr` getters are byte offsets into `WebAssembly.Memory`. vwp-v1 §3.3.2 lays the
//! actor block out as a struct of arrays precisely so a client can point a typed array at
//! a column, so that is what crosses: an offset and a count, no serialization. The views
//! stay valid until `generation` changes — which happens when a column grows or linear
//! memory does — and `js/replay.js` in this crate wraps that rule.

use wasm_bindgen::prelude::*;

use crate::session::{ReplaySession, Step};

/// This module's WebAssembly linear memory.
///
/// A caller needs it to turn the `*_ptr` getters into typed arrays:
/// `new Int32Array(wasmMemory().buffer, reader.xMmPtr, reader.actorCount)`. It is a
/// function rather than a cached value because linear memory grows, and growth detaches
/// the `ArrayBuffer` a cached reference would hand out.
#[wasm_bindgen(js_name = wasmMemory)]
pub fn wasm_memory() -> JsValue {
    wasm_bindgen::memory()
}

/// A replay reader over one recording.
#[wasm_bindgen]
#[derive(Debug)]
pub struct ReplayReader {
    inner: ReplaySession,
}

/// Turns a range list into the flat `[offset, len, …]` array JavaScript receives.
fn flatten(ranges: &[(u64, u64)]) -> Vec<f64> {
    let mut out = Vec::with_capacity(ranges.len() * 2);
    for (off, len) in ranges {
        out.push(*off as f64);
        out.push(*len as f64);
    }
    out
}

fn js_err(e: v2xw_record::error::RecordError) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl ReplayReader {
    /// Opens a recording that is already in memory, from an `ArrayBuffer`.
    ///
    /// # Errors
    /// Throws with the reader's own message if the file is not a recording this build can
    /// index.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<ReplayReader, JsValue> {
        ReplaySession::from_bytes(bytes)
            .map(|inner| ReplayReader { inner })
            .map_err(js_err)
    }

    /// Opens a recording of `total_bytes` that will be fetched by HTTP range request.
    ///
    /// `total_bytes` is the `Content-Length` of the recording, which a `HEAD` or the
    /// `Content-Range` of any range response reports.
    #[wasm_bindgen]
    pub fn ranged(total_bytes: f64) -> ReplayReader {
        let total = if total_bytes.is_finite() && total_bytes > 0.0 {
            total_bytes as u64
        } else {
            0
        };
        ReplayReader {
            inner: ReplaySession::ranged(total),
        }
    }

    /// Hands back a fetched range.
    #[wasm_bindgen]
    pub fn supply(&mut self, offset: f64, bytes: &[u8]) {
        let at = if offset.is_finite() && offset >= 0.0 {
            offset as u64
        } else {
            return;
        };
        self.inner.supply(at, bytes.to_vec());
    }

    /// Reads the footer, the summary and the message indexes.
    ///
    /// Returns the ranges still wanted, empty once the index is in hand.
    ///
    /// # Errors
    /// Throws if the file is refused.
    #[wasm_bindgen]
    pub fn prepare(&mut self) -> Result<Vec<f64>, JsValue> {
        match self.inner.open().map_err(js_err)? {
            Step::Done(()) => Ok(Vec::new()),
            Step::Need(r) => Ok(flatten(&r)),
        }
    }

    /// True once the index has been read.
    #[wasm_bindgen(getter, js_name = isOpen)]
    pub fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    /// Seeks to `seconds` of simulated time.
    ///
    /// Returns the ranges still wanted, empty once the seek has completed and the columns
    /// hold the state at that instant.
    ///
    /// # Errors
    /// Throws outside the recorded span, or if a chunk or a frame is refused.
    #[wasm_bindgen]
    pub fn seek(&mut self, seconds: f64) -> Result<Vec<f64>, JsValue> {
        self.seek_ns(crate::seconds_to_ns(seconds))
    }

    /// [`ReplayReader::seek`] with the target in nanoseconds.
    ///
    /// # Errors
    /// As [`ReplayReader::seek`].
    #[wasm_bindgen(js_name = seekNs)]
    pub fn seek_ns(&mut self, t_ns: crate::SimTime) -> Result<Vec<f64>, JsValue> {
        match self.inner.seek(t_ns).map_err(js_err)? {
            Step::Done(_) => Ok(Vec::new()),
            Step::Need(r) => Ok(flatten(&r)),
        }
    }

    /// The keyframe the last seek started from, in nanoseconds.
    #[wasm_bindgen(getter, js_name = keyframeTimeNs)]
    pub fn keyframe_time_ns(&self) -> crate::SimTime {
        self.inner.last_seek().map_or(0, |r| r.keyframe_time_ns)
    }

    /// The simulated time the columns are resolved to, in nanoseconds.
    #[wasm_bindgen(getter, js_name = positionNs)]
    pub fn position_ns(&self) -> crate::SimTime {
        self.inner.last_seek().map_or(0, |r| r.position_ns)
    }

    /// How many deltas the last seek applied.
    #[wasm_bindgen(getter, js_name = deltaCount)]
    pub fn delta_count(&self) -> u32 {
        self.inner.last_seek().map_or(0, |r| r.deltas as u32)
    }

    /// How many chunks the last seek read and decompressed.
    #[wasm_bindgen(getter, js_name = chunksRead)]
    pub fn chunks_read(&self) -> u32 {
        self.inner.last_seek().map_or(0, |r| r.chunks_read as u32)
    }

    /// The number of actor slots, which is the length of every actor column.
    #[wasm_bindgen(getter, js_name = actorCount)]
    pub fn actor_count(&self) -> u32 {
        self.inner.scene().actor_count() as u32
    }

    /// The number of signal heads.
    #[wasm_bindgen(getter, js_name = signalCount)]
    pub fn signal_count(&self) -> u32 {
        self.inner.scene().signal_count() as u32
    }

    /// Changes whenever the typed-array views a client holds have gone stale.
    #[wasm_bindgen(getter)]
    pub fn generation(&self) -> u32 {
        self.inner.scene().generation()
    }

    /// The quantisation origin in metres: east, north, up (§3.3.1).
    #[wasm_bindgen(getter)]
    pub fn origin(&self) -> Vec<f64> {
        self.inner.scene().origin().to_vec()
    }

    /// The first recorded snapshot time in nanoseconds, or 0 before the index is read.
    #[wasm_bindgen(getter, js_name = spanStartNs)]
    pub fn span_start_ns(&self) -> crate::SimTime {
        self.inner
            .span()
            .ok()
            .flatten()
            .map_or(0, |(start, _)| start)
    }

    /// The last recorded snapshot time in nanoseconds, or 0 before the index is read.
    #[wasm_bindgen(getter, js_name = spanEndNs)]
    pub fn span_end_ns(&self) -> crate::SimTime {
        self.inner.span().ok().flatten().map_or(0, |(_, end)| end)
    }

    /// How many ranges have been supplied — the round-trip cost of the scrub.
    #[wasm_bindgen(getter)]
    pub fn requests(&self) -> u32 {
        self.inner.requests()
    }

    /// How many bytes of the recording have been fetched.
    #[wasm_bindgen(getter, js_name = residentBytes)]
    pub fn resident_bytes(&self) -> f64 {
        self.inner.resident_bytes() as f64
    }

    /// The recording's size in bytes.
    #[wasm_bindgen(getter, js_name = totalBytes)]
    pub fn total_bytes(&self) -> f64 {
        self.inner.total_bytes() as f64
    }

    /// The byte offset in linear memory of the `u32[A]` actor-id column.
    #[wasm_bindgen(getter, js_name = actorIdPtr)]
    pub fn actor_id_ptr(&self) -> u32 {
        self.inner.scene().actor_id().as_ptr() as u32
    }

    /// The byte offset of the `i32[A]` millimetres-east column.
    #[wasm_bindgen(getter, js_name = xMmPtr)]
    pub fn x_mm_ptr(&self) -> u32 {
        self.inner.scene().x_mm().as_ptr() as u32
    }

    /// The byte offset of the `i32[A]` millimetres-north column.
    #[wasm_bindgen(getter, js_name = yMmPtr)]
    pub fn y_mm_ptr(&self) -> u32 {
        self.inner.scene().y_mm().as_ptr() as u32
    }

    /// The byte offset of the `i32[A]` millimetres-up column.
    #[wasm_bindgen(getter, js_name = zMmPtr)]
    pub fn z_mm_ptr(&self) -> u32 {
        self.inner.scene().z_mm().as_ptr() as u32
    }

    /// The byte offset of the `u32[A]` lane-id column. **Ground truth.**
    #[wasm_bindgen(getter, js_name = laneIdPtr)]
    pub fn lane_id_ptr(&self) -> u32 {
        self.inner.scene().lane_id().as_ptr() as u32
    }

    /// The byte offset of the `u16[A]` heading column, in binary radians.
    #[wasm_bindgen(getter, js_name = headingPtr)]
    pub fn heading_ptr(&self) -> u32 {
        self.inner.scene().heading_brad().as_ptr() as u32
    }

    /// The byte offset of the `i16[A]` speed column, in 1/128 m/s.
    #[wasm_bindgen(getter, js_name = speedPtr)]
    pub fn speed_ptr(&self) -> u32 {
        self.inner.scene().speed_cq().as_ptr() as u32
    }

    /// The byte offset of the `i16[A]` acceleration column, in 1/64 m/s². **Ground truth.**
    #[wasm_bindgen(getter, js_name = accelPtr)]
    pub fn accel_ptr(&self) -> u32 {
        self.inner.scene().accel_cq().as_ptr() as u32
    }

    /// The byte offset of the `u8[A]` class-index column.
    #[wasm_bindgen(getter, js_name = classIdxPtr)]
    pub fn class_idx_ptr(&self) -> u32 {
        self.inner.scene().class_idx().as_ptr() as u32
    }

    /// The byte offset of the `u8[A]` state column (§3.3.4).
    #[wasm_bindgen(getter, js_name = statePtr)]
    pub fn state_ptr(&self) -> u32 {
        self.inner.scene().state().as_ptr() as u32
    }

    /// The byte offset of the `u8[A]` verified-neighbour column.
    #[wasm_bindgen(getter, js_name = verifiedNeighborsPtr)]
    pub fn verified_neighbors_ptr(&self) -> u32 {
        self.inner.scene().verified_neighbors().as_ptr() as u32
    }

    /// The byte offset of the `u32[S]` signal-id column.
    #[wasm_bindgen(getter, js_name = signalIdPtr)]
    pub fn signal_id_ptr(&self) -> u32 {
        self.inner.scene().signal_id().as_ptr() as u32
    }

    /// The byte offset of the `u16[S]` time-to-change column, in deciseconds.
    #[wasm_bindgen(getter, js_name = signalTtcPtr)]
    pub fn signal_ttc_ptr(&self) -> u32 {
        self.inner.scene().signal_ttc_ds().as_ptr() as u32
    }

    /// The byte offset of the `u8[S]` phase column.
    #[wasm_bindgen(getter, js_name = signalPhasePtr)]
    pub fn signal_phase_ptr(&self) -> u32 {
        self.inner.scene().signal_phase().as_ptr() as u32
    }

    /// The byte offset of the recorded `Keyframe` frame the last seek returned.
    #[wasm_bindgen(getter, js_name = keyframePtr)]
    pub fn keyframe_ptr(&self) -> u32 {
        self.inner.keyframe_frame().map_or(0, |b| b.as_ptr() as u32)
    }

    /// The length in bytes of that frame, header included.
    #[wasm_bindgen(getter, js_name = keyframeLen)]
    pub fn keyframe_len(&self) -> u32 {
        self.inner.keyframe_frame().map_or(0, |b| b.len() as u32)
    }

    /// How many `Delta` frames the last seek returned.
    #[wasm_bindgen(getter, js_name = deltaFrames)]
    pub fn delta_frames(&self) -> u32 {
        self.inner.delta_frames() as u32
    }

    /// The byte offset of the `i`-th recorded `Delta` frame, in ascending time order.
    #[wasm_bindgen(js_name = deltaPtr)]
    pub fn delta_ptr(&self, i: u32) -> u32 {
        self.inner
            .delta_frame(i as usize)
            .map_or(0, |b| b.as_ptr() as u32)
    }

    /// The length in bytes of that frame, header included.
    #[wasm_bindgen(js_name = deltaLen)]
    pub fn delta_len(&self, i: u32) -> u32 {
        self.inner
            .delta_frame(i as usize)
            .map_or(0, |b| b.len() as u32)
    }
}
