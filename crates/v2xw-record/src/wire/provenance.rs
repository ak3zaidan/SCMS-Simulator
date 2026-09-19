//! `Provenance` (§3.8) — the `prov_id` dictionary and the metric dimension dictionary.
//!
//! Recorded because conformance C5 requires every `prov_id` a `MetricSample` or an event
//! payload references to have been delivered before it is first referenced, and a replay
//! reader can only honour that if the frames are in the file. The symbol-table extension
//! is carried verbatim: ids are never reassigned within a connection (§2.5), so the
//! reader appends the extension to the table `Hello` established.

use crate::error::Result;
use crate::wire::{Frame, MsgType, StrTable, get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};
use v2xw_core::time::SimTime;

/// The `Provenance` prefix is 32 bytes (§3.8).
pub const PREFIX_BYTES: usize = 32;
/// Bytes per provenance entry (§3.8).
pub const ENTRY_STRIDE: usize = 24;
/// Bytes per dimension entry (§3.8).
pub const DIM_STRIDE: usize = 8;

/// `flags` bit 0 — replace the whole table rather than appending (§3.8).
pub const PROV_REPLACE_ALL: u32 = 0x0000_0001;
/// `flags` bit 1 — no further `Provenance` frames will be sent (§3.8).
pub const PROV_FINAL: u32 = 0x0000_0002;

/// One provenance entry (§3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvEntry {
    /// Non-zero, unique for the run.
    pub prov_id: u32,
    /// String id of the model id.
    pub str_model_id: u32,
    /// String id of the model version.
    pub str_model_version: u32,
    /// String id of the content-addressed parameter-set id.
    pub str_param_set_id: u32,
    /// String id of the generated model-card URL.
    pub str_card_url: u32,
    /// Index into the model-card schema's `family` enum.
    pub family: u16,
    /// `0` value, `1` node, `2` link, `3` actor, `4` metric, `5` channel, `6` world.
    pub subject_kind: u16,
}

/// One dimension-dictionary entry (§3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DimEntry {
    /// The handle a `MetricSample.dim_key` references.
    pub dim_key: u32,
    /// String id of the canonical `"k=v,k=v"` with keys sorted ASCII-ascending.
    pub str_dims: u32,
}

/// A decoded `Provenance` body (§3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceBody {
    /// The simulated time the frame was produced.
    pub sim_time_ns: SimTime,
    /// The provenance entries.
    pub entries: Vec<ProvEntry>,
    /// The dimension dictionary entries.
    pub dims: Vec<DimEntry>,
    /// The symbol-table extension, whose entry `i` takes global id
    /// `table_size_before + i`.
    pub strings: Option<StrTable>,
    /// `PROV_REPLACE_ALL` / `PROV_FINAL`.
    pub flags: u32,
}

impl ProvenanceBody {
    /// The encoded body length in bytes.
    pub fn encoded_len(&self) -> usize {
        PREFIX_BYTES
            + ENTRY_STRIDE * self.entries.len()
            + DIM_STRIDE * self.dims.len()
            + self.strings.as_ref().map_or(0, StrTable::encoded_len)
    }

    /// Encodes the body (§3.8).
    pub fn encode(&self) -> Vec<u8> {
        let p = self.entries.len();
        let dk = self.dims.len();
        let mut out = vec![0u8; self.encoded_len()];
        let mut at = PREFIX_BYTES;
        let off_entries = if p > 0 { at } else { 0 };
        at += ENTRY_STRIDE * p;
        let off_dims = if dk > 0 { at } else { 0 };
        at += DIM_STRIDE * dk;
        let off_strings = if self.strings.is_some() { at } else { 0 };

        put_u64(&mut out, 0, self.sim_time_ns);
        put_u32(&mut out, 8, p as u32);
        put_u32(&mut out, 12, off_entries as u32);
        put_u32(&mut out, 16, dk as u32);
        put_u32(&mut out, 20, off_dims as u32);
        put_u32(&mut out, 24, off_strings as u32);
        put_u32(&mut out, 28, self.flags);

        let mut q = PREFIX_BYTES;
        for e in &self.entries {
            put_u32(&mut out, q, e.prov_id);
            q += 4;
        }
        for e in &self.entries {
            put_u32(&mut out, q, e.str_model_id);
            q += 4;
        }
        for e in &self.entries {
            put_u32(&mut out, q, e.str_model_version);
            q += 4;
        }
        for e in &self.entries {
            put_u32(&mut out, q, e.str_param_set_id);
            q += 4;
        }
        for e in &self.entries {
            put_u32(&mut out, q, e.str_card_url);
            q += 4;
        }
        for e in &self.entries {
            put_u16(&mut out, q, e.family);
            q += 2;
        }
        for e in &self.entries {
            put_u16(&mut out, q, e.subject_kind);
            q += 2;
        }
        for d in &self.dims {
            put_u32(&mut out, q, d.dim_key);
            q += 4;
        }
        for d in &self.dims {
            put_u32(&mut out, q, d.str_dims);
            q += 4;
        }
        if let Some(t) = &self.strings {
            t.encode_into(&mut out, off_strings);
        }
        out
    }

    /// The whole frame.
    ///
    /// # Errors
    /// [`crate::error::RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Provenance, seq, flags, &self.encode())
    }

    /// Decodes a body (§3.8).
    ///
    /// # Errors
    /// [`crate::error::RecordError::Truncated`] if a block runs off the end, or
    /// [`crate::error::RecordError::Malformed`] from the symbol table.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Provenance";
        let sim_time_ns = get_u64(body, 0, WHAT)?;
        let p = get_u32(body, 8, WHAT)? as usize;
        let off_entries = get_u32(body, 12, WHAT)? as usize;
        let dk = get_u32(body, 16, WHAT)? as usize;
        let off_dims = get_u32(body, 20, WHAT)? as usize;
        let off_strings = get_u32(body, 24, WHAT)? as usize;
        let flags = get_u32(body, 28, WHAT)?;
        let mut entries = Vec::with_capacity(p);
        for i in 0..p {
            entries.push(ProvEntry {
                prov_id: get_u32(body, off_entries + 4 * i, WHAT)?,
                str_model_id: get_u32(body, off_entries + 4 * p + 4 * i, WHAT)?,
                str_model_version: get_u32(body, off_entries + 8 * p + 4 * i, WHAT)?,
                str_param_set_id: get_u32(body, off_entries + 12 * p + 4 * i, WHAT)?,
                str_card_url: get_u32(body, off_entries + 16 * p + 4 * i, WHAT)?,
                family: get_u16(body, off_entries + 20 * p + 2 * i, WHAT)?,
                subject_kind: get_u16(body, off_entries + 22 * p + 2 * i, WHAT)?,
            });
        }
        let mut dims = Vec::with_capacity(dk);
        for i in 0..dk {
            dims.push(DimEntry {
                dim_key: get_u32(body, off_dims + 4 * i, WHAT)?,
                str_dims: get_u32(body, off_dims + 4 * dk + 4 * i, WHAT)?,
            });
        }
        let strings = if off_strings == 0 {
            None
        } else {
            Some(StrTable::decode(body, off_strings)?.0)
        };
        Ok(ProvenanceBody {
            sim_time_ns,
            entries,
            dims,
            strings,
            flags,
        })
    }
}
