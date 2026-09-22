//! `Telemetry` (§3.5) — per-node telemetry as an array of structs with a wire-carried
//! `record_size`.
//!
//! §3.5's `DECISION` explains the shape: the HUD and the inspector read one node at a
//! time, the batch is small, and a wire `record_size` is the cheapest forward-compatible
//! extension path (§8.4). A reader MUST stride by `record_size` rather than assume 208
//! (conformance C2), which is what [`TelemetryBody::decode`] does.
//!
//! Every field is populated or explicitly set to its unknown sentinel (conformance C1);
//! [`NodeTelemetry::unknown`] is that all-sentinel record, so a producer that models a
//! subset of the fields cannot leave the rest silently zero.

use crate::error::{RecordError, Result};
use crate::wire::{
    Frame, MsgType, U8_NONE, U16_NONE, U32_NONE, U64_NONE, get_f32, get_i16, get_i64, get_u8,
    get_u16, get_u32, get_u64, put_f32, put_i16, put_i64, put_u8, put_u16, put_u32, put_u64,
};
use v2xw_core::time::SimTime;

/// The `Telemetry` prefix is 32 bytes (§3.5.1).
pub const PREFIX_BYTES: usize = 32;

/// `record_size` in v1 (§3.5.1). A reader strides by the wire value, not by this.
pub const RECORD_SIZE_V1: u32 = 208;

/// The `NodeTelemetry` record of §3.5.2.
///
/// Units are exactly as the table gives them: bytes, KiB, per-mille, centi-dBm,
/// nanoseconds, milliseconds, 1/s. The two ground-truth fields are `clock_offset_ns` and
/// `pos_error_m`; the `node_state` value `6` (compromised) is ground truth as a *value*
/// (§5.2 blanks it to `2`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(missing_docs, clippy::struct_excessive_bools)]
#[non_exhaustive]
pub struct NodeTelemetry {
    pub storage_used_b: u64,
    pub storage_total_b: u64,
    pub next_topup_ns: u64,
    pub crl_bytes: u64,
    pub outbox_bytes: u64,
    /// Believed − true time. **Ground truth.**
    pub clock_offset_ns: i64,
    pub node_id: u32,
    pub ram_used_kib: u32,
    pub ram_total_kib: u32,
    pub drop_rx_overflow: u32,
    pub drop_verify_policy_skip: u32,
    pub drop_verify_overflow: u32,
    pub drop_tx_overflow: u32,
    pub drop_reassembly_timeout: u32,
    pub drop_crl_backlog: u32,
    pub cert_stored: u32,
    pub crl_entries: u32,
    pub outbox_msgs: u32,
    pub peer_cache_entries: u32,
    pub p2pcd_requests: u32,
    pub full_cert_msgs: u32,
    pub msgs_in_per_s: f32,
    pub msgs_out_per_s: f32,
    pub verifications_per_s: f32,
    pub verify_wait_p50_ms: f32,
    pub verify_wait_p95_ms: f32,
    pub gnss_hdop: f32,
    pub gnss_sigma_m: f32,
    pub clock_drift_ppm: f32,
    /// Horizontal belief error. **Ground truth.**
    pub pos_error_m: f32,
    pub airtime_ms_per_s: f32,
    pub cpu_util_pm: u16,
    pub hsm_util_pm: u16,
    pub q_rx_p50: u16,
    pub q_rx_p95: u16,
    pub q_verify_p50: u16,
    pub q_verify_p95: u16,
    pub q_app_p50: u16,
    pub q_app_p95: u16,
    pub q_tx_p50: u16,
    pub q_tx_p95: u16,
    pub q_crl_p50: u16,
    pub q_crl_p95: u16,
    pub dcc_state: u16,
    pub cbr_pm: u16,
    pub tx_power_cdbm: i16,
    pub nbr_total: u16,
    pub nbr_verified: u16,
    pub nbr_unverified: u16,
    pub nbr_revoked: u16,
    pub cert_active: u16,
    pub crl_expansion_pm: u16,
    pub unverified_ratio_pm: u16,
    pub gnss_fix: u8,
    pub node_state: u8,
    pub verify_policy: u8,
}

/// The declared grid of every `f32` field of §3.5.2, in field order (D9).
///
/// Rates and counts per second are on 1e-3, milliseconds on 1e-3, metres on 1e-3, ppm on
/// 1e-3, HDOP (a dimensionless ratio) on 1e-4.
pub const F32_GRIDS: [(&str, f64); 10] = [
    ("msgs_in_per_s", 1e-3),
    ("msgs_out_per_s", 1e-3),
    ("verifications_per_s", 1e-3),
    ("verify_wait_p50_ms", 1e-3),
    ("verify_wait_p95_ms", 1e-3),
    ("gnss_hdop", 1e-4),
    ("gnss_sigma_m", 1e-3),
    ("clock_drift_ppm", 1e-3),
    ("pos_error_m", 1e-3),
    ("airtime_ms_per_s", 1e-3),
];

fn q32(v: f32, quantum: f64) -> f32 {
    v2xw_core::math::quantize_to(f64::from(v), quantum) as f32
}

impl NodeTelemetry {
    /// The all-sentinel record for `node_id`: every counter unknown, every float `NaN`
    /// (§3.5.2's saturation rule, conformance C1).
    pub fn unknown(node_id: u32) -> Self {
        NodeTelemetry {
            storage_used_b: U64_NONE,
            storage_total_b: U64_NONE,
            next_topup_ns: U64_NONE,
            crl_bytes: U64_NONE,
            outbox_bytes: U64_NONE,
            clock_offset_ns: 0,
            node_id,
            ram_used_kib: U32_NONE,
            ram_total_kib: U32_NONE,
            drop_rx_overflow: U32_NONE,
            drop_verify_policy_skip: U32_NONE,
            drop_verify_overflow: U32_NONE,
            drop_tx_overflow: U32_NONE,
            drop_reassembly_timeout: U32_NONE,
            drop_crl_backlog: U32_NONE,
            cert_stored: U32_NONE,
            crl_entries: U32_NONE,
            outbox_msgs: U32_NONE,
            peer_cache_entries: U32_NONE,
            p2pcd_requests: U32_NONE,
            full_cert_msgs: U32_NONE,
            msgs_in_per_s: f32::NAN,
            msgs_out_per_s: f32::NAN,
            verifications_per_s: f32::NAN,
            verify_wait_p50_ms: f32::NAN,
            verify_wait_p95_ms: f32::NAN,
            gnss_hdop: f32::NAN,
            gnss_sigma_m: f32::NAN,
            clock_drift_ppm: f32::NAN,
            pos_error_m: f32::NAN,
            airtime_ms_per_s: f32::NAN,
            cpu_util_pm: U16_NONE,
            hsm_util_pm: U16_NONE,
            q_rx_p50: U16_NONE,
            q_rx_p95: U16_NONE,
            q_verify_p50: U16_NONE,
            q_verify_p95: U16_NONE,
            q_app_p50: U16_NONE,
            q_app_p95: U16_NONE,
            q_tx_p50: U16_NONE,
            q_tx_p95: U16_NONE,
            q_crl_p50: U16_NONE,
            q_crl_p95: U16_NONE,
            dcc_state: U16_NONE,
            cbr_pm: U16_NONE,
            tx_power_cdbm: i16::MIN,
            nbr_total: U16_NONE,
            nbr_verified: U16_NONE,
            nbr_unverified: U16_NONE,
            nbr_revoked: U16_NONE,
            cert_active: U16_NONE,
            crl_expansion_pm: U16_NONE,
            unverified_ratio_pm: U16_NONE,
            gnss_fix: U8_NONE,
            node_state: U8_NONE,
            verify_policy: U8_NONE,
        }
    }

    /// The record with every float on its declared grid — the writer-side quantisation
    /// gate of ADR 0004 §7 and build decision D9.
    pub fn quantised(&self) -> Self {
        let mut r = *self;
        r.msgs_in_per_s = q32(r.msgs_in_per_s, 1e-3);
        r.msgs_out_per_s = q32(r.msgs_out_per_s, 1e-3);
        r.verifications_per_s = q32(r.verifications_per_s, 1e-3);
        r.verify_wait_p50_ms = q32(r.verify_wait_p50_ms, 1e-3);
        r.verify_wait_p95_ms = q32(r.verify_wait_p95_ms, 1e-3);
        r.gnss_hdop = q32(r.gnss_hdop, 1e-4);
        r.gnss_sigma_m = q32(r.gnss_sigma_m, 1e-3);
        r.clock_drift_ppm = q32(r.clock_drift_ppm, 1e-3);
        r.pos_error_m = q32(r.pos_error_m, 1e-3);
        r.airtime_ms_per_s = q32(r.airtime_ms_per_s, 1e-3);
        r
    }

    /// The record's ten `f32` fields, paired with the grid each declares.
    pub fn f32_fields(&self) -> [(&'static str, f32, f64); 10] {
        [
            ("msgs_in_per_s", self.msgs_in_per_s, 1e-3),
            ("msgs_out_per_s", self.msgs_out_per_s, 1e-3),
            ("verifications_per_s", self.verifications_per_s, 1e-3),
            ("verify_wait_p50_ms", self.verify_wait_p50_ms, 1e-3),
            ("verify_wait_p95_ms", self.verify_wait_p95_ms, 1e-3),
            ("gnss_hdop", self.gnss_hdop, 1e-4),
            ("gnss_sigma_m", self.gnss_sigma_m, 1e-3),
            ("clock_drift_ppm", self.clock_drift_ppm, 1e-3),
            ("pos_error_m", self.pos_error_m, 1e-3),
            ("airtime_ms_per_s", self.airtime_ms_per_s, 1e-3),
        ]
    }

    fn encode_into(&self, out: &mut [u8], at: usize) {
        put_u64(out, at, self.storage_used_b);
        put_u64(out, at + 8, self.storage_total_b);
        put_u64(out, at + 16, self.next_topup_ns);
        put_u64(out, at + 24, self.crl_bytes);
        put_u64(out, at + 32, self.outbox_bytes);
        put_i64(out, at + 40, self.clock_offset_ns);
        put_u32(out, at + 48, self.node_id);
        put_u32(out, at + 52, self.ram_used_kib);
        put_u32(out, at + 56, self.ram_total_kib);
        put_u32(out, at + 60, self.drop_rx_overflow);
        put_u32(out, at + 64, self.drop_verify_policy_skip);
        put_u32(out, at + 68, self.drop_verify_overflow);
        put_u32(out, at + 72, self.drop_tx_overflow);
        put_u32(out, at + 76, self.drop_reassembly_timeout);
        put_u32(out, at + 80, self.drop_crl_backlog);
        put_u32(out, at + 84, self.cert_stored);
        put_u32(out, at + 88, self.crl_entries);
        put_u32(out, at + 92, self.outbox_msgs);
        put_u32(out, at + 96, self.peer_cache_entries);
        put_u32(out, at + 100, self.p2pcd_requests);
        put_u32(out, at + 104, self.full_cert_msgs);
        put_f32(out, at + 108, self.msgs_in_per_s);
        put_f32(out, at + 112, self.msgs_out_per_s);
        put_f32(out, at + 116, self.verifications_per_s);
        put_f32(out, at + 120, self.verify_wait_p50_ms);
        put_f32(out, at + 124, self.verify_wait_p95_ms);
        put_f32(out, at + 128, self.gnss_hdop);
        put_f32(out, at + 132, self.gnss_sigma_m);
        put_f32(out, at + 136, self.clock_drift_ppm);
        put_f32(out, at + 140, self.pos_error_m);
        put_f32(out, at + 144, self.airtime_ms_per_s);
        put_u16(out, at + 148, self.cpu_util_pm);
        put_u16(out, at + 150, self.hsm_util_pm);
        put_u16(out, at + 152, self.q_rx_p50);
        put_u16(out, at + 154, self.q_rx_p95);
        put_u16(out, at + 156, self.q_verify_p50);
        put_u16(out, at + 158, self.q_verify_p95);
        put_u16(out, at + 160, self.q_app_p50);
        put_u16(out, at + 162, self.q_app_p95);
        put_u16(out, at + 164, self.q_tx_p50);
        put_u16(out, at + 166, self.q_tx_p95);
        put_u16(out, at + 168, self.q_crl_p50);
        put_u16(out, at + 170, self.q_crl_p95);
        put_u16(out, at + 172, self.dcc_state);
        put_u16(out, at + 174, self.cbr_pm);
        put_i16(out, at + 176, self.tx_power_cdbm);
        put_u16(out, at + 178, self.nbr_total);
        put_u16(out, at + 180, self.nbr_verified);
        put_u16(out, at + 182, self.nbr_unverified);
        put_u16(out, at + 184, self.nbr_revoked);
        put_u16(out, at + 186, self.cert_active);
        put_u16(out, at + 188, self.crl_expansion_pm);
        put_u16(out, at + 190, self.unverified_ratio_pm);
        put_u8(out, at + 192, self.gnss_fix);
        put_u8(out, at + 193, self.node_state);
        put_u8(out, at + 194, self.verify_policy);
        // at+195 `reserved8` and at+196..at+208 `reserved` stay zero (§8.4).
    }

    fn decode_at(body: &[u8], at: usize) -> Result<Self> {
        const WHAT: &str = "vwp NodeTelemetry";
        Ok(NodeTelemetry {
            storage_used_b: get_u64(body, at, WHAT)?,
            storage_total_b: get_u64(body, at + 8, WHAT)?,
            next_topup_ns: get_u64(body, at + 16, WHAT)?,
            crl_bytes: get_u64(body, at + 24, WHAT)?,
            outbox_bytes: get_u64(body, at + 32, WHAT)?,
            clock_offset_ns: get_i64(body, at + 40, WHAT)?,
            node_id: get_u32(body, at + 48, WHAT)?,
            ram_used_kib: get_u32(body, at + 52, WHAT)?,
            ram_total_kib: get_u32(body, at + 56, WHAT)?,
            drop_rx_overflow: get_u32(body, at + 60, WHAT)?,
            drop_verify_policy_skip: get_u32(body, at + 64, WHAT)?,
            drop_verify_overflow: get_u32(body, at + 68, WHAT)?,
            drop_tx_overflow: get_u32(body, at + 72, WHAT)?,
            drop_reassembly_timeout: get_u32(body, at + 76, WHAT)?,
            drop_crl_backlog: get_u32(body, at + 80, WHAT)?,
            cert_stored: get_u32(body, at + 84, WHAT)?,
            crl_entries: get_u32(body, at + 88, WHAT)?,
            outbox_msgs: get_u32(body, at + 92, WHAT)?,
            peer_cache_entries: get_u32(body, at + 96, WHAT)?,
            p2pcd_requests: get_u32(body, at + 100, WHAT)?,
            full_cert_msgs: get_u32(body, at + 104, WHAT)?,
            msgs_in_per_s: get_f32(body, at + 108, WHAT)?,
            msgs_out_per_s: get_f32(body, at + 112, WHAT)?,
            verifications_per_s: get_f32(body, at + 116, WHAT)?,
            verify_wait_p50_ms: get_f32(body, at + 120, WHAT)?,
            verify_wait_p95_ms: get_f32(body, at + 124, WHAT)?,
            gnss_hdop: get_f32(body, at + 128, WHAT)?,
            gnss_sigma_m: get_f32(body, at + 132, WHAT)?,
            clock_drift_ppm: get_f32(body, at + 136, WHAT)?,
            pos_error_m: get_f32(body, at + 140, WHAT)?,
            airtime_ms_per_s: get_f32(body, at + 144, WHAT)?,
            cpu_util_pm: get_u16(body, at + 148, WHAT)?,
            hsm_util_pm: get_u16(body, at + 150, WHAT)?,
            q_rx_p50: get_u16(body, at + 152, WHAT)?,
            q_rx_p95: get_u16(body, at + 154, WHAT)?,
            q_verify_p50: get_u16(body, at + 156, WHAT)?,
            q_verify_p95: get_u16(body, at + 158, WHAT)?,
            q_app_p50: get_u16(body, at + 160, WHAT)?,
            q_app_p95: get_u16(body, at + 162, WHAT)?,
            q_tx_p50: get_u16(body, at + 164, WHAT)?,
            q_tx_p95: get_u16(body, at + 166, WHAT)?,
            q_crl_p50: get_u16(body, at + 168, WHAT)?,
            q_crl_p95: get_u16(body, at + 170, WHAT)?,
            dcc_state: get_u16(body, at + 172, WHAT)?,
            cbr_pm: get_u16(body, at + 174, WHAT)?,
            tx_power_cdbm: get_i16(body, at + 176, WHAT)?,
            nbr_total: get_u16(body, at + 178, WHAT)?,
            nbr_verified: get_u16(body, at + 180, WHAT)?,
            nbr_unverified: get_u16(body, at + 182, WHAT)?,
            nbr_revoked: get_u16(body, at + 184, WHAT)?,
            cert_active: get_u16(body, at + 186, WHAT)?,
            crl_expansion_pm: get_u16(body, at + 188, WHAT)?,
            unverified_ratio_pm: get_u16(body, at + 190, WHAT)?,
            gnss_fix: get_u8(body, at + 192, WHAT)?,
            node_state: get_u8(body, at + 193, WHAT)?,
            verify_policy: get_u8(body, at + 194, WHAT)?,
        })
    }
}

/// A decoded `Telemetry` body (§3.5).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryBody {
    /// The end of the sampling window.
    pub sim_time_ns: SimTime,
    /// The length of the sampling window.
    pub window_ns: u64,
    /// The `record_size` on the wire; readers stride by this (conformance C2).
    pub record_size: u32,
    /// One record per subscribed node.
    pub records: Vec<NodeTelemetry>,
}

impl TelemetryBody {
    /// A body at the v1 record size.
    pub fn new(sim_time_ns: SimTime, window_ns: u64, records: Vec<NodeTelemetry>) -> Self {
        TelemetryBody {
            sim_time_ns,
            window_ns,
            record_size: RECORD_SIZE_V1,
            records,
        }
    }

    /// The encoded body length in bytes.
    pub fn encoded_len(&self) -> usize {
        PREFIX_BYTES + self.record_size as usize * self.records.len()
    }

    /// Encodes the body (§3.5), quantising every float to its declared grid (D9).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.encoded_len()];
        put_u64(&mut out, 0, self.sim_time_ns);
        put_u64(&mut out, 8, self.window_ns);
        put_u32(&mut out, 16, self.records.len() as u32);
        put_u32(
            &mut out,
            20,
            if self.records.is_empty() {
                0
            } else {
                PREFIX_BYTES as u32
            },
        );
        put_u32(&mut out, 24, self.record_size);
        for (i, r) in self.records.iter().enumerate() {
            r.quantised()
                .encode_into(&mut out, PREFIX_BYTES + i * self.record_size as usize);
        }
        out
    }

    /// The whole frame.
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Telemetry, seq, flags, &self.encode())
    }

    /// Decodes a body, striding by the wire `record_size` (conformance C2).
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the records run off the end, or
    /// [`RecordError::Malformed`] if `record_size` is smaller than v1's prefix of known
    /// fields — a reader may stride further than it understands but never less.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Telemetry";
        let sim_time_ns = get_u64(body, 0, WHAT)?;
        let window_ns = get_u64(body, 8, WHAT)?;
        let n = get_u32(body, 16, WHAT)? as usize;
        let off_records = get_u32(body, 20, WHAT)? as usize;
        let record_size = get_u32(body, 24, WHAT)?;
        if n > 0 && record_size < RECORD_SIZE_V1 {
            return Err(RecordError::malformed(
                WHAT,
                format!("record_size = {record_size} is smaller than v1's {RECORD_SIZE_V1}"),
            ));
        }
        if n > 0 && off_records % 8 != 0 {
            return Err(RecordError::malformed(
                WHAT,
                format!("off_records = {off_records} must be 8-aligned (§3.5.1)"),
            ));
        }
        // `n` is a wire `u32` about to size an allocation of `NodeTelemetry`, which is a
        // wide struct; `record_size` is the exact stride and is already known to be at
        // least v1's, so it is also the bound.
        let n = crate::wire::checked_count(
            n,
            (record_size as usize).max(RECORD_SIZE_V1 as usize),
            body.len(),
            WHAT,
            "node_count",
        )?;
        let mut records = Vec::with_capacity(n);
        for i in 0..n {
            // Bounded by the check above: `n * record_size <= body.len()`.
            records.push(NodeTelemetry::decode_at(
                body,
                off_records + i * record_size as usize,
            )?);
        }
        Ok(TelemetryBody {
            sim_time_ns,
            window_ns,
            record_size,
            records,
        })
    }
}
