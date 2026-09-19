//! `Event` (§3.6) — a batch of typed records on the channels of 03-interfaces §14.
//!
//! The container treats a payload as opaque bytes, which is exactly right: §3.6.1's whole
//! point is that "a reader that does not know a `channel_id` skips it using
//! `payload_len`", so new channels are added without a version bump (§8.4). The fields
//! the `NODE-only` profile has to blank are poked by offset in [`crate::profile`], from
//! §5.2's table, and nothing else in this crate needs to know a payload's shape.

use crate::error::{RecordError, Result};
use crate::wire::{Frame, MsgType, ceil8, get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};
use v2xw_core::time::SimTime;

/// The `Event` prefix is 32 bytes (§3.6.1).
pub const PREFIX_BYTES: usize = 32;
/// Bytes per index entry (§3.6.1).
pub const INDEX_STRIDE: usize = 16;

/// One event in a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEntry {
    /// The event's simulated time.
    pub sim_time_ns: SimTime,
    /// The channel id of §3.6.2.
    pub channel_id: u16,
    /// The payload, whose length is a multiple of eight for every channel v1 defines.
    pub payload: Vec<u8>,
}

/// A decoded `Event` body (§3.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventBody {
    /// Inclusive lower bound of the batch.
    pub t_start_ns: SimTime,
    /// Inclusive upper bound of the batch.
    pub t_end_ns: SimTime,
    /// The events, sorted by `(sim_time_ns, channel_id)` when encoded (conformance C4).
    pub entries: Vec<EventEntry>,
}

impl EventBody {
    /// A batch spanning `[t_start, t_end]`.
    pub fn new(t_start_ns: SimTime, t_end_ns: SimTime, entries: Vec<EventEntry>) -> Self {
        EventBody {
            t_start_ns,
            t_end_ns,
            entries,
        }
    }

    /// The channel ids present, ascending and deduplicated.
    ///
    /// The recorder splits a mixed batch by channel so that per-channel message indexes
    /// work (§7.1).
    pub fn channel_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.entries.iter().map(|e| e.channel_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The batch restricted to one channel, or `None` if it holds nothing on it.
    pub fn only_channel(&self, channel_id: u16) -> Option<EventBody> {
        let entries: Vec<EventEntry> = self
            .entries
            .iter()
            .filter(|e| e.channel_id == channel_id)
            .cloned()
            .collect();
        if entries.is_empty() {
            return None;
        }
        let t_start = entries
            .iter()
            .map(|e| e.sim_time_ns)
            .min()
            .unwrap_or(self.t_start_ns);
        let t_end = entries
            .iter()
            .map(|e| e.sim_time_ns)
            .max()
            .unwrap_or(self.t_end_ns);
        Some(EventBody {
            t_start_ns: t_start,
            t_end_ns: t_end,
            entries,
        })
    }

    /// Encodes the body (§3.6), sorting the index and padding every payload to eight.
    pub fn encode(&self) -> Vec<u8> {
        let mut sorted: Vec<&EventEntry> = self.entries.iter().collect();
        // Stable, so two events with the same (time, channel) keep their emission order.
        sorted.sort_by_key(|e| (e.sim_time_ns, e.channel_id));
        let e = sorted.len();
        let off_index = if e > 0 { PREFIX_BYTES } else { 0 };
        let off_payloads = PREFIX_BYTES + INDEX_STRIDE * e;
        let payload_bytes: usize = sorted.iter().map(|x| ceil8(x.payload.len())).sum();
        let mut out = vec![0u8; off_payloads + payload_bytes];
        put_u64(&mut out, 0, self.t_start_ns);
        put_u64(&mut out, 8, self.t_end_ns);
        put_u32(&mut out, 16, e as u32);
        put_u32(&mut out, 20, off_index as u32);
        put_u32(
            &mut out,
            24,
            if payload_bytes > 0 {
                off_payloads as u32
            } else {
                0
            },
        );
        put_u32(&mut out, 28, payload_bytes as u32);

        let mut rel = 0usize;
        for (i, entry) in sorted.iter().enumerate() {
            let padded = ceil8(entry.payload.len());
            put_u64(&mut out, PREFIX_BYTES + 8 * i, entry.sim_time_ns);
            put_u32(&mut out, PREFIX_BYTES + 8 * e + 4 * i, rel as u32);
            put_u16(&mut out, PREFIX_BYTES + 12 * e + 2 * i, padded as u16);
            put_u16(&mut out, PREFIX_BYTES + 14 * e + 2 * i, entry.channel_id);
            let at = off_payloads + rel;
            out[at..at + entry.payload.len()].copy_from_slice(&entry.payload);
            rel += padded;
        }
        out
    }

    /// The whole frame.
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Event, seq, flags, &self.encode())
    }

    /// Decodes a body (§3.6).
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the index or a payload runs off the end, or
    /// [`RecordError::Malformed`] if an offset is not 8-aligned or the index is not sorted
    /// (conformance C4).
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Event";
        let t_start_ns = get_u64(body, 0, WHAT)?;
        let t_end_ns = get_u64(body, 8, WHAT)?;
        let e = get_u32(body, 16, WHAT)? as usize;
        let off_index = get_u32(body, 20, WHAT)? as usize;
        let off_payloads = get_u32(body, 24, WHAT)? as usize;
        let payload_bytes = get_u32(body, 28, WHAT)? as usize;
        if e == 0 {
            return Ok(EventBody {
                t_start_ns,
                t_end_ns,
                entries: Vec::new(),
            });
        }
        if off_index % 8 != 0 || off_payloads % 8 != 0 {
            return Err(RecordError::malformed(
                WHAT,
                format!(
                    "off_index = {off_index} and off_payloads = {off_payloads} must be 8-aligned"
                ),
            ));
        }
        let mut entries = Vec::with_capacity(e);
        let mut prev = (0u64, 0u16);
        for i in 0..e {
            let sim_time_ns = get_u64(body, off_index + 8 * i, WHAT)?;
            let payload_off = get_u32(body, off_index + 8 * e + 4 * i, WHAT)? as usize;
            let payload_len = get_u16(body, off_index + 12 * e + 2 * i, WHAT)? as usize;
            let channel_id = get_u16(body, off_index + 14 * e + 2 * i, WHAT)?;
            if i > 0 && (sim_time_ns, channel_id) < prev {
                return Err(RecordError::malformed(
                    WHAT,
                    format!(
                        "index entry {i} breaks the (sim_time_ns, channel_id) ordering (§3.6.1)"
                    ),
                ));
            }
            prev = (sim_time_ns, channel_id);
            if payload_off % 8 != 0 {
                return Err(RecordError::malformed(
                    WHAT,
                    format!("payload_off = {payload_off} of entry {i} must be a multiple of 8"),
                ));
            }
            if payload_off + payload_len > payload_bytes {
                return Err(RecordError::Truncated {
                    what: "vwp Event.payload",
                    at: payload_off,
                    need: payload_len,
                    have: payload_bytes.saturating_sub(payload_off),
                });
            }
            let at = off_payloads + payload_off;
            crate::wire::need_pub(body, at, payload_len, "vwp Event.payload")?;
            entries.push(EventEntry {
                sim_time_ns,
                channel_id,
                payload: body[at..at + payload_len].to_vec(),
            });
        }
        Ok(EventBody {
            t_start_ns,
            t_end_ns,
            entries,
        })
    }
}
