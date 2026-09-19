//! VWP v1 binary framing — `docs/protocol/vwp-v1.md` §2 and §3.
//!
//! The recording stores the bytes that went over the wire, so this crate has to *be* able
//! to produce and read those bytes: the writer builds them (§3.3, §3.4), the recorder
//! stores them untouched (§7.1), the replay reader hands them back, and the `NODE-only`
//! profile is the one transformation that decodes, blanks and re-encodes them (§5).
//!
//! # What "verbatim" buys
//!
//! §7.2's guarantee — live and replay frames are byte-identical once
//! [`TRANSPORT_FLAG_MASK`] bits are masked off — is only provable because nothing between
//! the producer and the file re-encodes anything. [`Frame`] is therefore a `Vec<u8>` with
//! accessors, not a struct that happens to serialise back to the same bytes; the decoded
//! forms ([`snapshot::KeyframeBody`], [`snapshot::DeltaBody`]) exist for inspection,
//! verification and blanking, and the round-trip between the two is itself a test.
//!
//! # Reading is checked, always
//!
//! Every accessor here bounds-checks. A recording is read after a crash more often than
//! before one, and a decoder that indexes a slice directly turns a truncated file into a
//! panic — which is the one failure mode a forensic tool must not have.

pub mod event;
pub mod hello;
pub mod metric;
pub mod provenance;
pub mod snapshot;
pub mod telemetry;

use crate::error::{RecordError, Result};

/// `magic` of §2.1: `0x31505756`, whose little-endian wire bytes are `V W P 1`.
pub const MAGIC: u32 = 0x3150_5756;

/// The frame header is 24 bytes; the body starts at frame offset 24 (§2.1).
pub const HEADER_BYTES: usize = 24;

/// The protocol major version this build speaks (§8.1).
pub const VERSION_MAJOR: u16 = 1;

/// The protocol minor version this build writes (§8.1).
pub const VERSION_MINOR: u16 = 0;

/// `FLAG_COMPRESSED` (§2.3) — the body is a zstd frame. Transport.
pub const FLAG_COMPRESSED: u16 = 0x0001;
/// `FLAG_RESYNC` (§2.3) — this keyframe re-seeds interpolation state. Transport.
pub const FLAG_RESYNC: u16 = 0x0002;
/// `FLAG_NODE_ONLY` (§2.3) — produced under the `node` profile. Canonical.
pub const FLAG_NODE_ONLY: u16 = 0x0004;
/// `FLAG_END_OF_RUN` (§2.3) — the last canonical frame of the run. Canonical.
pub const FLAG_END_OF_RUN: u16 = 0x0008;
/// `FLAG_CONTINUED` (§2.3) — one of several frames carrying one logical unit. Transport.
pub const FLAG_CONTINUED: u16 = 0x0010;
/// `FLAG_SEEK_RESULT` (§2.3) — this keyframe answers a `run.seek`. Transport.
pub const FLAG_SEEK_RESULT: u16 = 0x0020;

/// The flag bits that are part of the recorded frame (§2.3). §7.2's byte-identity
/// guarantee is stated modulo this mask.
pub const CANONICAL_FLAG_MASK: u16 = 0x000C;

/// The flag bits the *sender* owns, which the recorder clears before storing (§7.2 item 3).
pub const TRANSPORT_FLAG_MASK: u16 = 0x0033;

/// The `u32` "absent" sentinel of §0.
pub const U32_NONE: u32 = 0xFFFF_FFFF;
/// The `u16` "absent" sentinel of §0.
pub const U16_NONE: u16 = 0xFFFF;
/// The `u8` "absent" sentinel of §0.
pub const U8_NONE: u8 = 0xFF;
/// The `u64` "absent" sentinel of §0 — used for times.
pub const U64_NONE: u64 = u64::MAX;

/// The message types of §2.4.
///
/// `Hello`, `Error` and `Bye` are connection frames: they carry the seq the *next*
/// canonical frame will have and do not consume one. `Error` and `Bye` never reach a
/// recording (§0.1: they are connection-scoped, not part of the canonical stream), so
/// this crate names them for completeness and the recorder refuses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MsgType {
    /// `0x0001` — connection preamble (§3.1).
    Hello,
    /// `0x0002` — a full snapshot (§3.3).
    Keyframe,
    /// `0x0003` — one mobility step of change (§3.4).
    Delta,
    /// `0x0004` — per-node telemetry (§3.5).
    Telemetry,
    /// `0x0005` — a batch of typed event records (§3.6).
    Event,
    /// `0x0006` — aggregated metric samples (§3.7).
    MetricSample,
    /// `0x0007` — provenance and the dimension dictionary (§3.8).
    Provenance,
    /// `0x0008` — inline world bytes for static hosting (§3.9).
    WorldChunk,
    /// `0x00FE` — a stream-level error (§3.10). Never recorded.
    Error,
    /// `0x00FF` — end of stream (§3.11). Never recorded.
    Bye,
}

impl MsgType {
    /// The wire id of §2.4.
    pub const fn id(self) -> u16 {
        match self {
            MsgType::Hello => 0x0001,
            MsgType::Keyframe => 0x0002,
            MsgType::Delta => 0x0003,
            MsgType::Telemetry => 0x0004,
            MsgType::Event => 0x0005,
            MsgType::MetricSample => 0x0006,
            MsgType::Provenance => 0x0007,
            MsgType::WorldChunk => 0x0008,
            MsgType::Error => 0x00FE,
            MsgType::Bye => 0x00FF,
        }
    }

    /// The type with this wire id, or `None` for one this build does not know.
    ///
    /// A reader "MUST ignore a frame whose `msg_type` it does not know" (§2.1), which is
    /// why this returns an option rather than an error (conformance F2).
    pub const fn from_id(id: u16) -> Option<Self> {
        Some(match id {
            0x0001 => MsgType::Hello,
            0x0002 => MsgType::Keyframe,
            0x0003 => MsgType::Delta,
            0x0004 => MsgType::Telemetry,
            0x0005 => MsgType::Event,
            0x0006 => MsgType::MetricSample,
            0x0007 => MsgType::Provenance,
            0x0008 => MsgType::WorldChunk,
            0x00FE => MsgType::Error,
            0x00FF => MsgType::Bye,
            _ => return None,
        })
    }

    /// True if the frame is canonical: it consumes a `seq` and belongs to the recorded
    /// stream (§0.1, §2.4).
    pub const fn is_canonical(self) -> bool {
        matches!(
            self,
            MsgType::Keyframe
                | MsgType::Delta
                | MsgType::Telemetry
                | MsgType::Event
                | MsgType::MetricSample
                | MsgType::Provenance
                | MsgType::WorldChunk
        )
    }

    /// The name the MCAP schema record carries, `vwp.v1.<Type>` (§7.1).
    pub const fn schema_name(self) -> &'static str {
        match self {
            MsgType::Hello => "vwp.v1.Hello",
            MsgType::Keyframe => "vwp.v1.Keyframe",
            MsgType::Delta => "vwp.v1.Delta",
            MsgType::Telemetry => "vwp.v1.Telemetry",
            MsgType::Event => "vwp.v1.Event",
            MsgType::MetricSample => "vwp.v1.MetricSample",
            MsgType::Provenance => "vwp.v1.Provenance",
            MsgType::WorldChunk => "vwp.v1.WorldChunk",
            MsgType::Error => "vwp.v1.Error",
            MsgType::Bye => "vwp.v1.Bye",
        }
    }
}

/// A decoded frame header (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// The protocol major version.
    pub version: u16,
    /// The message type id, kept raw so an unknown one can still be skipped (§2.1).
    pub msg_type: u16,
    /// The length of the **uncompressed** body.
    pub body_len: u32,
    /// The frame flags (§2.3).
    pub flags: u16,
    /// The reserved `u16` at `@14`, as it was on the wire.
    ///
    /// Written as zero by this build and **ignored** on read (§0, conformance F6). It is
    /// surfaced rather than discarded because §8.5 makes it the place a minor version adds
    /// a field, so a reader that knows v1.1 can look here without this crate having to
    /// know what it will mean.
    pub reserved: u16,
    /// The canonical sequence number (§1.4).
    pub seq: u64,
}

impl FrameHeader {
    /// The message type, or `None` if this build does not know it.
    pub const fn kind(&self) -> Option<MsgType> {
        MsgType::from_id(self.msg_type)
    }
}

/// One complete VWP frame: the 24-byte header followed by the uncompressed body.
///
/// This is the unit the recorder stores (§7.1's `DECISION`: "the MCAP message payload is
/// the whole frame including the 24-byte header") and the unit the byte-identity
/// guarantee is written over.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    bytes: Vec<u8>,
}

impl Frame {
    /// Builds a frame from a message type, a sequence number, flags and a body.
    ///
    /// # Errors
    /// [`RecordError::Unrepresentable`] if the body is longer than `u32::MAX`.
    pub fn new(msg_type: MsgType, seq: u64, flags: u16, body: &[u8]) -> Result<Self> {
        let body_len = u32::try_from(body.len()).map_err(|_| RecordError::Unrepresentable {
            what: "frame body_len",
            value: body.len().to_string(),
            ty: "u32",
        })?;
        let mut bytes = vec![0u8; HEADER_BYTES + body.len()];
        put_u32(&mut bytes, 0, MAGIC);
        put_u16(&mut bytes, 4, VERSION_MAJOR);
        put_u16(&mut bytes, 6, msg_type.id());
        put_u32(&mut bytes, 8, body_len);
        put_u16(&mut bytes, 12, flags);
        put_u16(&mut bytes, 14, 0);
        put_u64(&mut bytes, 16, seq);
        bytes[HEADER_BYTES..].copy_from_slice(body);
        Ok(Frame { bytes })
    }

    /// Adopts bytes that are already a frame, checking the header against the body.
    ///
    /// A non-zero reserved word and a body longer than this build's layout are both
    /// accepted: they are how §8.4 and §8.5 add a field in a minor version, and F6 requires
    /// reserved bytes to be ignored rather than refused.
    ///
    /// # Errors
    /// [`RecordError::BadMagic`] (conformance F1), [`RecordError::UnsupportedVersion`]
    /// (§8.6), [`RecordError::Truncated`] if the bytes are shorter than a header, and
    /// [`RecordError::Malformed`] if `body_len` disagrees with the bytes present
    /// (conformance F3).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let f = Frame { bytes };
        let h = f.header()?;
        if h.version != VERSION_MAJOR {
            return Err(RecordError::UnsupportedVersion {
                found: h.version,
                supported: VERSION_MAJOR,
            });
        }
        if h.flags & FLAG_COMPRESSED != 0 {
            return Err(RecordError::malformed(
                "vwp frame",
                "FLAG_COMPRESSED is set: the recorder stores uncompressed bodies (§7.1)",
            ));
        }
        let have = f.bytes.len() - HEADER_BYTES;
        if have != h.body_len as usize {
            return Err(RecordError::malformed(
                "vwp frame",
                format!("body_len = {} but {have} body bytes present", h.body_len),
            ));
        }
        Ok(f)
    }

    /// The decoded header (§2.1).
    ///
    /// The reserved `u16` at `@14` is **read and ignored**, not checked. §0 is in two
    /// halves — "reserved bytes MUST be written as zero by the sender and MUST be ignored
    /// by the reader" — and conformance F6 asks for both; this crate writes zero
    /// ([`Frame::new`]) and ignores what it reads. §8.5 makes that load-bearing: a minor
    /// version adds a field "in bytes that a v1 reader is already required to ignore: a
    /// `reserved` field", so rejecting a non-zero one would make every v1.1 frame
    /// unreadable by this build rather than merely degraded, breaking F6 and N1. Its value
    /// is carried in [`FrameHeader::reserved`] for a reader that knows what it means.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if there are fewer than 24 bytes, or
    /// [`RecordError::BadMagic`] if the magic is wrong.
    pub fn header(&self) -> Result<FrameHeader> {
        let magic = get_u32(&self.bytes, 0, "vwp frame header")?;
        if magic != MAGIC {
            return Err(RecordError::BadMagic {
                found: magic,
                expected: MAGIC,
            });
        }
        Ok(FrameHeader {
            version: get_u16(&self.bytes, 4, "vwp frame header")?,
            msg_type: get_u16(&self.bytes, 6, "vwp frame header")?,
            body_len: get_u32(&self.bytes, 8, "vwp frame header")?,
            flags: get_u16(&self.bytes, 12, "vwp frame header")?,
            reserved: get_u16(&self.bytes, 14, "vwp frame header")?,
            seq: get_u64(&self.bytes, 16, "vwp frame header")?,
        })
    }

    /// The frame's bytes, header included.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The frame's bytes, consuming the frame.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The body, without the 24-byte header.
    pub fn body(&self) -> &[u8] {
        &self.bytes[HEADER_BYTES.min(self.bytes.len())..]
    }

    /// The same frame with its flags replaced.
    ///
    /// Used in two places and nowhere else: the recorder clears
    /// [`TRANSPORT_FLAG_MASK`] before storing (§7.2 item 3), and the seek path sets
    /// `FLAG_RESYNC | FLAG_SEEK_RESULT` on the keyframe it emits (§7.3 step 8).
    pub fn with_flags(&self, flags: u16) -> Frame {
        let mut bytes = self.bytes.clone();
        if bytes.len() >= HEADER_BYTES {
            put_u16(&mut bytes, 12, flags);
        }
        Frame { bytes }
    }

    /// The same frame carrying a different sequence number.
    ///
    /// Used only where a *new producer* takes over a stream: the `NODE-only` stripper
    /// renumbers because it withholds frames and §1.4 requires `seq` to be dense.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the bytes are not a frame.
    pub fn renumbered(&self, seq: u64) -> Result<Frame> {
        let mut bytes = self.bytes.clone();
        need(&bytes, 16, 8, "vwp frame header")?;
        put_u64(&mut bytes, 16, seq);
        Ok(Frame { bytes })
    }

    /// The frame as the byte-identity guarantee compares it: transport flag bits cleared
    /// (§7.2).
    pub fn canonical(&self) -> Frame {
        let flags = self.header().map(|h| h.flags).unwrap_or(0);
        self.with_flags(flags & CANONICAL_FLAG_MASK)
    }

    /// The frame's sequence number (§1.4).
    ///
    /// # Errors
    /// Whatever [`Frame::header`] returns.
    pub fn seq(&self) -> Result<u64> {
        Ok(self.header()?.seq)
    }

    /// The simulated time this frame belongs at, which is what the recorder writes as the
    /// MCAP `log_time` (§7.1).
    ///
    /// Every canonical body but `Event` opens with `sim_time_ns`; an `Event` batch opens
    /// with `t_start_ns` and `t_end_ns`, and its position in a time index is `t_end_ns`,
    /// the step boundary at which the batch was produced.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the body is shorter than its prefix, or
    /// [`RecordError::Malformed`] for a message type that carries no time.
    pub fn sim_time(&self) -> Result<v2xw_core::time::SimTime> {
        let h = self.header()?;
        let body = self.body();
        match h.kind() {
            Some(MsgType::Event) => get_u64(body, 8, "vwp Event.t_end_ns"),
            Some(MsgType::Hello) => get_u64(body, 144, "vwp Hello.sim_time_ns"),
            Some(MsgType::WorldChunk) | None => Err(RecordError::malformed(
                "vwp frame",
                format!("message type {:#06x} carries no sim time", h.msg_type),
            )),
            Some(_) => get_u64(body, 0, "vwp body.sim_time_ns"),
        }
    }
}

// ---------------------------------------------------------------------------
// Checked little-endian accessors. Everything on the wire is little-endian (§0).
// ---------------------------------------------------------------------------

fn need(buf: &[u8], at: usize, n: usize, what: &'static str) -> Result<()> {
    let end = at.checked_add(n).ok_or(RecordError::Truncated {
        what,
        at,
        need: n,
        have: 0,
    })?;
    if end > buf.len() {
        return Err(RecordError::Truncated {
            what,
            at,
            need: n,
            have: buf.len().saturating_sub(at),
        });
    }
    Ok(())
}

/// Bounds check, for the modules that read a variable-length block themselves.
///
/// # Errors
/// [`RecordError::Truncated`] if `buf` has fewer than `n` bytes at `at`.
pub(crate) fn need_pub(buf: &[u8], at: usize, n: usize, what: &'static str) -> Result<()> {
    need(buf, at, n, what)
}

macro_rules! getter {
    ($name:ident, $ty:ty, $n:literal) => {
        /// Reads a little-endian value at `at`, bounds-checked.
        ///
        /// # Errors
        /// [`RecordError::Truncated`] if the buffer ends first.
        pub fn $name(buf: &[u8], at: usize, what: &'static str) -> Result<$ty> {
            need(buf, at, $n, what)?;
            let mut a = [0u8; $n];
            a.copy_from_slice(&buf[at..at + $n]);
            Ok(<$ty>::from_le_bytes(a))
        }
    };
}

getter!(get_u8, u8, 1);
getter!(get_i8, i8, 1);
getter!(get_u16, u16, 2);
getter!(get_i16, i16, 2);
getter!(get_u32, u32, 4);
getter!(get_i32, i32, 4);
getter!(get_u64, u64, 8);
getter!(get_i64, i64, 8);
getter!(get_f32, f32, 4);
getter!(get_f64, f64, 8);

macro_rules! putter {
    ($name:ident, $ty:ty, $n:literal) => {
        /// Writes a little-endian value at `at`.
        ///
        /// # Panics
        /// If the buffer is too short. Writers size their buffer from the layout tables
        /// before filling it, so a short buffer is a bug in this crate, not bad input.
        pub fn $name(buf: &mut [u8], at: usize, v: $ty) {
            buf[at..at + $n].copy_from_slice(&v.to_le_bytes());
        }
    };
}

putter!(put_u8, u8, 1);
putter!(put_u16, u16, 2);
putter!(put_i16, i16, 2);
putter!(put_u32, u32, 4);
putter!(put_i32, i32, 4);
putter!(put_u64, u64, 8);
putter!(put_i64, i64, 8);
putter!(put_f32, f32, 4);
putter!(put_f64, f64, 8);

/// Reads a fixed-size byte array, bounds-checked.
///
/// # Errors
/// [`RecordError::Truncated`] if the buffer ends first.
pub fn get_bytes<const N: usize>(buf: &[u8], at: usize, what: &'static str) -> Result<[u8; N]> {
    need(buf, at, N, what)?;
    let mut a = [0u8; N];
    a.copy_from_slice(&buf[at..at + N]);
    Ok(a)
}

/// Rounds up to the next multiple of four — §2.2's padding rule.
pub const fn ceil4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Rounds up to the next multiple of eight — §3.6.1's payload rule.
pub const fn ceil8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// The symbol table of §2.5: `n`, `blob_bytes`, `n + 1` offsets, then the UTF-8 blob
/// padded to a multiple of four.
///
/// Ids are indices into this table and id `0` is always the empty string, so a writer
/// puts `""` first. The table is append-only within a connection; a recording holds the
/// one table `Hello` established plus whatever `Provenance` frames appended (§3.8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrTable {
    /// The strings, in id order.
    pub strings: Vec<String>,
}

impl StrTable {
    /// An empty table holding only the mandatory `""` at id 0.
    pub fn new() -> Self {
        StrTable {
            strings: vec![String::new()],
        }
    }

    /// The id of `s`, appending it if it is not already interned.
    ///
    /// Linear in the table size, which is what a per-run table of a few hundred strings
    /// wants; the alternative is a hash map, whose iteration order would have to be kept
    /// out of the output anyway.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|t| t == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    /// The string with this id, or `None`.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.strings.get(id as usize).map(String::as_str)
    }

    /// The encoded size in bytes.
    pub fn encoded_len(&self) -> usize {
        let blob: usize = self.strings.iter().map(String::len).sum();
        8 + 4 * (self.strings.len() + 1) + ceil4(blob)
    }

    /// Writes the table into `out` at `at` (§2.5).
    ///
    /// # Panics
    /// If `out` is shorter than `at + self.encoded_len()`.
    pub fn encode_into(&self, out: &mut [u8], at: usize) {
        let blob_bytes: usize = self.strings.iter().map(String::len).sum();
        put_u32(out, at, self.strings.len() as u32);
        put_u32(out, at + 4, blob_bytes as u32);
        let mut cursor = 0usize;
        for (i, s) in self.strings.iter().enumerate() {
            put_u32(out, at + 8 + 4 * i, cursor as u32);
            cursor += s.len();
        }
        put_u32(out, at + 8 + 4 * self.strings.len(), blob_bytes as u32);
        let mut p = at + 8 + 4 * (self.strings.len() + 1);
        for s in &self.strings {
            out[p..p + s.len()].copy_from_slice(s.as_bytes());
            p += s.len();
        }
    }

    /// The table as its own byte block.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.encoded_len()];
        self.encode_into(&mut out, 0);
        out
    }

    /// Decodes a table at `at`, returning it and its encoded length.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the buffer ends inside the table,
    /// [`RecordError::Malformed`] if the offsets are not non-decreasing, do not start at
    /// zero, do not end at `blob_bytes`, or cut a UTF-8 sequence.
    pub fn decode(buf: &[u8], at: usize) -> Result<(StrTable, usize)> {
        const WHAT: &str = "vwp StrTable";
        let n = get_u32(buf, at, WHAT)? as usize;
        let blob_bytes = get_u32(buf, at + 4, WHAT)? as usize;
        let mut offsets = Vec::with_capacity(n + 1);
        for i in 0..=n {
            offsets.push(get_u32(buf, at + 8 + 4 * i, WHAT)? as usize);
        }
        if offsets[0] != 0 || offsets[n] != blob_bytes {
            return Err(RecordError::malformed(
                WHAT,
                format!(
                    "offsets[0] = {} and offsets[{n}] = {} must be 0 and blob_bytes = {blob_bytes}",
                    offsets[0], offsets[n]
                ),
            ));
        }
        let blob_at = at + 8 + 4 * (n + 1);
        need(buf, blob_at, blob_bytes, WHAT)?;
        let blob = &buf[blob_at..blob_at + blob_bytes];
        let mut strings = Vec::with_capacity(n);
        for i in 0..n {
            let (a, b) = (offsets[i], offsets[i + 1]);
            if a > b || b > blob_bytes {
                return Err(RecordError::malformed(
                    WHAT,
                    format!("offsets[{i}] = {a} .. {b} is not a non-decreasing range"),
                ));
            }
            let s = std::str::from_utf8(&blob[a..b]).map_err(|e| {
                RecordError::malformed(WHAT, format!("string {i} is not UTF-8: {e}"))
            })?;
            strings.push(s.to_string());
        }
        Ok((StrTable { strings }, 8 + 4 * (n + 1) + ceil4(blob_bytes)))
    }
}
