//! `MetricSample` (§3.7) — aggregated metric values, an array of structs with a wire
//! `record_size`.
//!
//! The wire carries no unit and no quantum: those come from the `MetricDef`
//! (08-measurement-and-data.md §2). The *grid* is nevertheless part of the field's
//! contract, because D9 forbids a raw double reaching a recorded artefact, so this crate
//! declares it: [`crate::grid::Q_METRIC_VALUE`], and the writer applies it.

use crate::error::{RecordError, Result};
use crate::wire::{
    Frame, MsgType, get_f64, get_u8, get_u16, get_u32, get_u64, put_f64, put_u8, put_u16, put_u32,
    put_u64,
};
use v2xw_core::time::SimTime;

/// The `MetricSample` prefix is 32 bytes (§3.7).
pub const PREFIX_BYTES: usize = 32;

/// `record_size` in v1 (§3.7).
pub const RECORD_SIZE_V1: u32 = 32;

/// One metric sample (§3.7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricRow {
    /// The aggregated value, on [`crate::grid::Q_METRIC_VALUE`].
    pub value: f64,
    /// String id of the `MetricDef.name`.
    pub str_metric: u32,
    /// Handle into the dimension dictionary; `0` means no dimensions.
    pub dim_key: u32,
    /// The node this sample is for, `0xFFFFFFFF` if not per-node.
    pub node_id: u32,
    /// The number of underlying observations.
    pub count: u32,
    /// `MetricAgg`: 0 sum, 1 mean, 2 p50, 3 p95, 4 p99, 5 ratio, 6 rate, 7 max, 8 min.
    pub agg: u16,
    /// `Visibility`: 0 GT, 1 NODE, 2 PUBLIC, 3 DERIVED. A GT sample is not emitted under
    /// the `node` profile (§3.7, §5.2).
    pub visibility: u8,
    /// Provenance id; `0` none.
    pub prov_id: u32,
}

/// A decoded `MetricSample` body (§3.7).
#[derive(Debug, Clone, PartialEq)]
pub struct MetricBody {
    /// The **end** of the time bin.
    pub sim_time_ns: SimTime,
    /// The bin width.
    pub bin_width_ns: u64,
    /// The `record_size` on the wire.
    pub record_size: u32,
    /// The samples.
    pub samples: Vec<MetricRow>,
}

impl MetricBody {
    /// A body at the v1 record size.
    pub fn new(sim_time_ns: SimTime, bin_width_ns: u64, samples: Vec<MetricRow>) -> Self {
        MetricBody {
            sim_time_ns,
            bin_width_ns,
            record_size: RECORD_SIZE_V1,
            samples,
        }
    }

    /// The encoded body length in bytes.
    pub fn encoded_len(&self) -> usize {
        PREFIX_BYTES + self.record_size as usize * self.samples.len()
    }

    /// Encodes the body (§3.7), quantising every value to
    /// [`crate::grid::Q_METRIC_VALUE`] (D9).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.encoded_len()];
        put_u64(&mut out, 0, self.sim_time_ns);
        put_u64(&mut out, 8, self.bin_width_ns);
        put_u32(&mut out, 16, self.samples.len() as u32);
        put_u32(
            &mut out,
            20,
            if self.samples.is_empty() {
                0
            } else {
                PREFIX_BYTES as u32
            },
        );
        put_u32(&mut out, 24, self.record_size);
        for (i, r) in self.samples.iter().enumerate() {
            let at = PREFIX_BYTES + i * self.record_size as usize;
            put_f64(
                &mut out,
                at,
                v2xw_core::math::quantize_to(r.value, crate::grid::Q_METRIC_VALUE),
            );
            put_u32(&mut out, at + 8, r.str_metric);
            put_u32(&mut out, at + 12, r.dim_key);
            put_u32(&mut out, at + 16, r.node_id);
            put_u32(&mut out, at + 20, r.count);
            put_u16(&mut out, at + 24, r.agg);
            put_u8(&mut out, at + 26, r.visibility);
            put_u32(&mut out, at + 28, r.prov_id);
        }
        out
    }

    /// The whole frame.
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::MetricSample, seq, flags, &self.encode())
    }

    /// Decodes a body, striding by the wire `record_size`.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] or [`RecordError::Malformed`] as for
    /// [`crate::wire::telemetry::TelemetryBody::decode`].
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp MetricSample";
        let sim_time_ns = get_u64(body, 0, WHAT)?;
        let bin_width_ns = get_u64(body, 8, WHAT)?;
        let m = get_u32(body, 16, WHAT)? as usize;
        let off_samples = get_u32(body, 20, WHAT)? as usize;
        let record_size = get_u32(body, 24, WHAT)?;
        if m > 0 && record_size < RECORD_SIZE_V1 {
            return Err(RecordError::malformed(
                WHAT,
                format!("record_size = {record_size} is smaller than v1's {RECORD_SIZE_V1}"),
            ));
        }
        if m > 0 && off_samples % 8 != 0 {
            return Err(RecordError::malformed(
                WHAT,
                format!("off_samples = {off_samples} must be 8-aligned (§3.7)"),
            ));
        }
        // `m` is a wire `u32` about to size an allocation, and `record_size` — already
        // checked to be at least v1's — is the exact stride, so it is also the right bound.
        let m = crate::wire::checked_count(
            m,
            (record_size as usize).max(RECORD_SIZE_V1 as usize),
            body.len(),
            WHAT,
            "sample_count",
        )?;
        let mut samples = Vec::with_capacity(m);
        for i in 0..m {
            // Bounded by the check above: `m * record_size <= body.len()`.
            let at = off_samples + i * record_size as usize;
            samples.push(MetricRow {
                value: get_f64(body, at, WHAT)?,
                str_metric: get_u32(body, at + 8, WHAT)?,
                dim_key: get_u32(body, at + 12, WHAT)?,
                node_id: get_u32(body, at + 16, WHAT)?,
                count: get_u32(body, at + 20, WHAT)?,
                agg: get_u16(body, at + 24, WHAT)?,
                visibility: get_u8(body, at + 26, WHAT)?,
                prov_id: get_u32(body, at + 28, WHAT)?,
            });
        }
        Ok(MetricBody {
            sim_time_ns,
            bin_width_ns,
            record_size,
            samples,
        })
    }
}
