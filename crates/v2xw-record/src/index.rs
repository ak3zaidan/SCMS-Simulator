//! The seek index: MCAP's footer, summary, chunk index and per-channel message index —
//! §7.3 steps 1–4, and the "index reader" ADR 0008 §4 asks for.
//!
//! # Why this is hand-rolled on top of the `mcap` crate
//!
//! `mcap::Summary::read` needs a slice of the whole file, because it seeks inside one.
//! That is fine for a test fixture and wrong for the thing §7.4 budgets: "read 1–2 chunks
//! from disk". This module therefore reads the 20-byte footer, then the summary section,
//! then each chunk's message-index records, and then — per seek — only the chunk records
//! it actually needs, through a [`Source`] that is either a file handle or a buffer. Each
//! record is still parsed by [`mcap::read::parse_record`], so the format knowledge stays
//! in the MCAP crate; only *which bytes to fetch* lives here.
//!
//! The record framing this module depends on is MCAP's own and is fixed by the
//! specification: every record is `opcode (1 byte) || length (u64 LE) || body`.
//!
//! # Nothing here allocates on an unvalidated length
//!
//! `error.rs` promises that malformed input comes back as a named variant rather than a
//! panic, and this module is where that promise was false. [`SeekIndex::for_each_message_in_chunk`]
//! used to hand the chunk record's own header straight to `mcap::read::ChunkReader`, which
//! reserves a buffer of the wire-controlled `uncompressed_size` and then reads each inner
//! record's length out of the *decompressed* bytes and reserves that too — so a single
//! flipped bit inside a zstd payload produced a multi-exabyte `reserve_exact` and a
//! SIGABRT that `catch_unwind` cannot trap, and a chunk header claiming
//! `uncompressed_size = 1` produced an arithmetic overflow. Both are reachable from every
//! read path.
//!
//! Three things close it, in this order:
//!
//! 1. the chunk record's decoded header is cross-checked against the chunk **index**,
//!    which already carries the authoritative `uncompressed_size`, `compression` and time
//!    span for that chunk, and against [`MAX_CHUNK_UNCOMPRESSED_BYTES`];
//! 2. the chunk's CRC is verified **before** any record inside it is parsed
//!    (`prevalidate_chunk_crcs`), so a corrupt payload is a `Chunk CRC failed` rather than
//!    a length read out of nonsense; and
//! 3. every record length, inner or outer, is capped at the chunk's own validated
//!    uncompressed size (`record_length_limit`), because no record inside a chunk can be
//!    longer than the chunk it is in.
//!
//! `tests/fuzz_corrupt.rs` flips and truncates across a whole recording and asserts the
//! promise.
//!
//! # Nothing here does unchecked arithmetic on a wire value either
//!
//! The same promise has a second half, and it was false here too. The chunk-index bounds
//! check read `if c.file_offset + c.record_length > size`, an unchecked 64-bit addition of
//! two numbers taken straight out of the chunk index. It failed differently in each build
//! profile: a `file_offset` of `u64::MAX` panicked in debug, and in release the addition
//! wrapped, the sum came out small, the check *passed* and the corrupt file was accepted.
//! Release is what people run, so the shipped behaviour was silent acceptance — the exact
//! failure a bounds check exists to prevent.
//!
//! Every offset, length, count and capacity this module derives from a recording is
//! therefore computed with checked arithmetic and refused as
//! [`RecordError::WireOverflow`] if it does not fit, including the two siblings that are
//! bounded today only by someone else's invariant: `offset + RECORD_PREFIX` in
//! `SeekIndex::read_message_indexes`, which is guarded only by the saturating comparison
//! inside the two [`Source`] implementations, and the chunk record's `compressed_size`,
//! which is guarded only by `mcap::read::parse_record`. `tests/overflow.rs` runs the
//! chunk-index case in **both** profiles, because the two behaviours differ and release is
//! the dangerous one.
//!
//! # What was checked is reported, never assumed
//!
//! A chunk's CRC can be switched off from the wire — `uncompressed_crc = 0` is the
//! container's "not present" — so [`SeekIndex::for_each_message_in_chunk`] returns
//! [`ChunkIntegrity`] rather than `()`. The policy, and the reasoning behind choosing it
//! over refusing and over ignoring, is on [`ChunkIntegrity`].

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use mcap::records::{self, op};

use crate::error::{RecordError, Result};

/// MCAP's file magic, at both ends of the file.
pub const MCAP_MAGIC: [u8; 8] = [0x89, b'M', b'C', b'A', b'P', 0x30, b'\r', b'\n'];

/// The size of a record's `opcode || length` prefix.
pub const RECORD_PREFIX: usize = 9;

/// The footer record's body: `summary_start`, `summary_offset_start`, `summary_crc`.
pub const FOOTER_BODY: usize = 20;

/// The largest uncompressed chunk this reader will decompress, as a ceiling on any
/// allocation derived from a recording's own bytes.
///
/// §7.1 targets 4 MiB and [`crate::writer::RecordingWriter`] holds a chunk to
/// `chunk_target_bytes` plus one message, so 256 MiB is two orders of magnitude of head
/// room for a file this crate wrote and still an allocation a machine survives. A file
/// that genuinely needs a larger chunk is refused with a named error, which is the right
/// answer: the alternative is trusting a number a bit flip can choose.
pub const MAX_CHUNK_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Whether a chunk's contents were actually checked against a stored checksum.
///
/// # Why this is a return value and not an assumption
///
/// MCAP protects a chunk with a CRC-32 over its uncompressed records, and this reader
/// verifies it *before* parsing anything inside the chunk. But the container defines
/// `uncompressed_crc = 0` as "no CRC is available", so the field is also the switch that
/// turns the check off — and the switch is four bytes of the file, which is to say it is
/// under the control of whoever supplied the file. Zeroing it skips validation entirely.
///
/// # The policy, and why
///
/// **A chunk without a checksum is read, and the fact that nothing was checked is
/// reported.** Neither of the alternatives is right:
///
/// * *Refusing it* would reject a conforming file. A zero CRC is explicitly legal, and a
///   producer that streams into a chunk it cannot rewind to cannot compute one. This
///   crate would be the tool that cannot open half the files in the ecosystem.
/// * *Accepting it silently* is what was wrong before, and it is the worse failure. Every
///   recording [`crate::writer::RecordingWriter`] produces carries a CRC on every chunk,
///   so a zero on read means an unusual producer or tampering, and the one behaviour that
///   must never happen is this crate calling a file verified when it checked nothing.
///
/// So the check is reported rather than assumed. [`SeekIndex::for_each_message_in_chunk`]
/// returns which of the two happened, [`crate::VerifyReport::chunks_without_checksum`]
/// counts them across a whole recording and
/// [`crate::VerifyReport::integrity_verified`] is false as soon as one is found, and a
/// caller that would rather refuse — an archive ingest, a conformance kit — turns on
/// [`crate::Reader::require_chunk_checksums`] and gets
/// [`RecordError::UncheckedChunk`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkIntegrity {
    /// The chunk declared a non-zero CRC-32 and it matched, checked before any record
    /// inside the chunk was parsed.
    Verified,
    /// The chunk declared `uncompressed_crc = 0`, the container's "not present", so
    /// nothing about its contents was checked. The records were still read under every
    /// other guard — the header cross-check against the chunk index, the
    /// [`MAX_CHUNK_UNCOMPRESSED_BYTES`] ceiling and the per-record length cap — but no
    /// checksum stands behind them.
    NotChecked,
}

impl ChunkIntegrity {
    /// True only for [`ChunkIntegrity::Verified`].
    pub const fn is_verified(self) -> bool {
        matches!(self, ChunkIntegrity::Verified)
    }
}

/// Somewhere bytes can be read from by offset.
///
/// Two implementations: a file, for a recording too large to hold in memory and for the
/// seek benchmark, and a buffer, for a recording already in memory and for tests.
pub trait Source {
    /// The total size in bytes.
    fn size(&self) -> u64;

    /// Reads exactly `len` bytes at `offset`.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the range runs past the end, or
    /// [`RecordError::Io`] if the read fails.
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>>;
}

/// A recording held in memory.
#[derive(Debug, Clone)]
pub struct MemorySource {
    bytes: Vec<u8>,
}

impl MemorySource {
    /// Wraps a buffer.
    pub fn new(bytes: Vec<u8>) -> Self {
        MemorySource { bytes }
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Source for MemorySource {
    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(len);
        if end > self.bytes.len() {
            return Err(RecordError::Truncated {
                what: "mcap file",
                at: start,
                need: len,
                have: self.bytes.len().saturating_sub(start),
            });
        }
        Ok(self.bytes[start..end].to_vec())
    }
}

/// A recording read from a file handle, a range at a time.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    size: u64,
    path: PathBuf,
}

impl FileSource {
    /// Opens a recording without reading it.
    ///
    /// # Errors
    /// [`RecordError::Io`] if the file cannot be opened or its length read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|e| RecordError::io(&path, e))?;
        let size = file
            .metadata()
            .map_err(|e| RecordError::io(&path, e))?
            .len();
        Ok(FileSource { file, size, path })
    }

    /// The path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset.saturating_add(len as u64) > self.size {
            return Err(RecordError::Truncated {
                what: "mcap file",
                at: usize::try_from(offset).unwrap_or(usize::MAX),
                need: len,
                have: usize::try_from(self.size.saturating_sub(offset)).unwrap_or(usize::MAX),
            });
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| RecordError::io(&self.path, e))?;
        let mut buf = vec![0u8; len];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| RecordError::io(&self.path, e))?;
        Ok(buf)
    }
}

/// One channel as the summary declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMeta {
    /// The MCAP channel id.
    pub id: u16,
    /// The topic, e.g. `"vwp/keyframe"` or `"record/node.tx"`.
    pub topic: String,
    /// The message encoding, `"vwp1"` or `"json"`.
    pub message_encoding: String,
    /// The schema id, `0` if the channel has none.
    pub schema_id: u16,
    /// The channel metadata, which is where this crate puts the visibility tag.
    pub metadata: BTreeMap<String, String>,
}

impl ChannelMeta {
    /// The visibility tag the recorder wrote, if any.
    pub fn visibility(&self) -> Option<&str> {
        self.metadata.get("visibility").map(String::as_str)
    }

    /// True if the recorder tagged this channel as carrying ground truth.
    pub fn is_ground_truth(&self) -> bool {
        self.metadata.get("ground_truth").map(String::as_str) == Some("true")
    }
}

/// One schema record as the summary declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaMeta {
    /// The schema id.
    pub id: u16,
    /// `vwp.v1.<Type>` for a frame channel (§7.1), or a JSON Schema name for a record
    /// channel.
    pub name: String,
    /// `"vwp1"` or `"jsonschema"`.
    pub encoding: String,
    /// The schema text — the layout tables for a frame channel, so the file is
    /// self-describing (§7.1).
    pub data: Vec<u8>,
}

/// One chunk, as the chunk index describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpan {
    /// The earliest message time in the chunk.
    pub message_start_time: u64,
    /// The latest message time in the chunk.
    pub message_end_time: u64,
    /// The chunk record's file offset.
    pub file_offset: u64,
    /// The chunk record's length, prefix included.
    pub record_length: u64,
    /// The uncompressed size of the chunk's records.
    pub uncompressed_size: u64,
    /// The compression used, `"zstd"` for a recording this crate wrote.
    pub compression: String,
    /// Where each channel's message-index record for this chunk lives.
    pub message_index_offsets: BTreeMap<u16, u64>,
}

/// One message's position: its time and the chunk that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageSlot {
    /// The message's `log_time`, which for this crate is its `sim_time_ns` (§7.1).
    pub sim_time: u64,
    /// The index into [`SeekIndex::chunks`].
    pub chunk: u32,
    /// The record's offset inside the chunk's uncompressed records.
    pub offset: u64,
}

/// The whole index: what a seek needs before it touches a chunk.
#[derive(Debug, Clone, Default)]
pub struct SeekIndex {
    /// Channels by id.
    pub channels: BTreeMap<u16, ChannelMeta>,
    /// Schemas by id.
    pub schemas: BTreeMap<u16, SchemaMeta>,
    /// Chunks in file order.
    pub chunks: Vec<ChunkSpan>,
    /// `vwp/keyframe` messages, ascending by time.
    pub keyframes: Vec<MessageSlot>,
    /// `vwp/delta` messages, ascending by time.
    pub deltas: Vec<MessageSlot>,
    /// The statistics record, if the writer emitted one.
    pub statistics: Option<records::Statistics>,
    /// Metadata records by name, as `(offset, length)`.
    pub metadata: BTreeMap<String, (u64, u64)>,
    /// Attachments by name, as `(offset, length)`.
    pub attachments: BTreeMap<String, (u64, u64)>,
}

impl SeekIndex {
    /// Reads the footer, the summary and every chunk's message index.
    ///
    /// This is §7.3 steps 1–3, and it is done once per file; a seek then does step 4 as a
    /// binary search over [`SeekIndex::keyframes`].
    ///
    /// # Errors
    /// [`RecordError::Truncated`] for a file shorter than a footer,
    /// [`RecordError::Malformed`] for bad magic or a footer that points outside the file,
    /// [`RecordError::NoSummary`] for a recording that was never finished, and
    /// [`RecordError::Mcap`] for a record the MCAP crate rejects.
    pub fn read(src: &mut impl Source) -> Result<Self> {
        let size = src.size();
        let min = (MCAP_MAGIC.len() + RECORD_PREFIX + FOOTER_BODY + MCAP_MAGIC.len()) as u64;
        if size < min {
            return Err(RecordError::Truncated {
                what: "mcap file",
                at: 0,
                need: min as usize,
                have: size as usize,
            });
        }
        let head = src.read_at(0, MCAP_MAGIC.len())?;
        let tail = src.read_at(size - MCAP_MAGIC.len() as u64, MCAP_MAGIC.len())?;
        if head != MCAP_MAGIC || tail != MCAP_MAGIC {
            return Err(RecordError::malformed(
                "mcap file",
                "the file does not start and end with the MCAP magic",
            ));
        }
        // The footer record is the last record before the closing magic.
        let footer_body_at = size - MCAP_MAGIC.len() as u64 - FOOTER_BODY as u64;
        let footer = src.read_at(footer_body_at, FOOTER_BODY)?;
        let summary_start = u64::from_le_bytes(footer[0..8].try_into().unwrap_or([0; 8]));
        if summary_start == 0 {
            return Err(RecordError::NoSummary);
        }
        let footer_record_at = footer_body_at - RECORD_PREFIX as u64;
        if summary_start >= footer_record_at {
            return Err(RecordError::malformed(
                "mcap footer",
                format!("summary_start = {summary_start} is not before the footer record"),
            ));
        }
        let summary_len = usize::try_from(footer_record_at - summary_start).unwrap_or(0);
        let summary = src.read_at(summary_start, summary_len)?;

        let mut index = SeekIndex::default();
        for (op_code, body) in records_in(&summary, "mcap summary")? {
            match mcap::read::parse_record(op_code, body)? {
                records::Record::Channel(c) => {
                    index.channels.insert(
                        c.id,
                        ChannelMeta {
                            id: c.id,
                            topic: c.topic,
                            message_encoding: c.message_encoding,
                            schema_id: c.schema_id,
                            metadata: c.metadata.into_iter().collect(),
                        },
                    );
                }
                records::Record::Schema { header, data } => {
                    index.schemas.insert(
                        header.id,
                        SchemaMeta {
                            id: header.id,
                            name: header.name,
                            encoding: header.encoding,
                            data: data.into_owned(),
                        },
                    );
                }
                records::Record::ChunkIndex(ci) => {
                    index.chunks.push(ChunkSpan {
                        message_start_time: ci.message_start_time,
                        message_end_time: ci.message_end_time,
                        file_offset: ci.chunk_start_offset,
                        record_length: ci.chunk_length,
                        uncompressed_size: ci.uncompressed_size,
                        compression: ci.compression,
                        message_index_offsets: ci.message_index_offsets,
                    });
                }
                records::Record::Statistics(s) => index.statistics = Some(s),
                records::Record::MetadataIndex(m) => {
                    index.metadata.insert(m.name, (m.offset, m.length));
                }
                records::Record::AttachmentIndex(a) => {
                    index.attachments.insert(a.name, (a.offset, a.length));
                }
                _ => {}
            }
        }
        index.chunks.sort_by_key(|c| c.file_offset);
        for c in &index.chunks {
            // Both operands come straight out of the chunk index, so the file chooses
            // them. `c.file_offset + c.record_length` was wrong in both build profiles at
            // once: it panicked in debug on `file_offset = u64::MAX`, and in release it
            // *wrapped* — the sum became small, this very check passed, and the corrupt
            // file was accepted. Release is what people run, so the production behaviour
            // was silent acceptance. `checked_add` makes the overflow the rejection.
            let end = c.file_offset.checked_add(c.record_length).ok_or_else(|| {
                RecordError::wire_overflow(
                    "mcap chunk index",
                    format!(
                        "chunk at {} declares a length of {}, and {} + {} does not fit in 64 bits, so                          the chunk index cannot be describing a real file",
                        c.file_offset, c.record_length, c.file_offset, c.record_length
                    ),
                )
            })?;
            if end > size {
                return Err(RecordError::malformed(
                    "mcap chunk index",
                    format!(
                        "chunk at {} runs {} bytes past the end of a {size}-byte file",
                        c.file_offset,
                        end - size
                    ),
                ));
            }
        }
        index.read_message_indexes(src)?;
        Ok(index)
    }

    /// The channel id of a topic, if the recording has it.
    pub fn channel_id(&self, topic: &str) -> Option<u16> {
        self.channels
            .values()
            .find(|c| c.topic == topic)
            .map(|c| c.id)
    }

    fn read_message_indexes(&mut self, src: &mut impl Source) -> Result<()> {
        let Some(kf) = self.channel_id(crate::writer::TOPIC_KEYFRAME) else {
            return Ok(());
        };
        let delta = self.channel_id(crate::writer::TOPIC_DELTA);
        let mut keyframes = Vec::new();
        let mut deltas = Vec::new();
        for (i, chunk) in self.chunks.iter().enumerate() {
            for (channel_id, offset) in &chunk.message_index_offsets {
                let want_kf = *channel_id == kf;
                let want_delta = delta == Some(*channel_id);
                if !want_kf && !want_delta {
                    continue;
                }
                // The sibling of the chunk-index overflow above, and the reason it is
                // checked here rather than argued about: `offset` is a wire value, and
                // `offset + RECORD_PREFIX` can only overflow when `offset` is within nine
                // bytes of `u64::MAX`. Today the `read_at` on the line below refuses
                // every such offset first, because both `Source` implementations compare
                // a *saturating* `offset + len` against the file size — but that is an
                // invariant of two other types, reachable only by reading them, and a
                // `Source` written elsewhere that did not saturate would hand this line a
                // wrapped offset pointing at byte 8 of the file. One `checked_add` costs
                // nothing and does not depend on anyone else's arithmetic.
                let body_at = offset.checked_add(RECORD_PREFIX as u64).ok_or_else(|| {
                    RecordError::wire_overflow(
                        "mcap message index",
                        format!(
                            "a message-index record at {offset} plus its {RECORD_PREFIX}-byte prefix does                              not fit in 64 bits"
                        ),
                    )
                })?;
                let prefix = src.read_at(*offset, RECORD_PREFIX)?;
                let len = u64::from_le_bytes(prefix[1..9].try_into().unwrap_or([0; 8]));
                let len = usize::try_from(len).unwrap_or(0);
                let body = src.read_at(body_at, len)?;
                let records::Record::MessageIndex(mi) = mcap::read::parse_record(prefix[0], &body)?
                else {
                    return Err(RecordError::malformed(
                        "mcap message index",
                        format!("the record at {offset} is not a message index"),
                    ));
                };
                let target = if want_kf { &mut keyframes } else { &mut deltas };
                for e in mi.records {
                    target.push(MessageSlot {
                        sim_time: e.log_time,
                        chunk: i as u32,
                        offset: e.offset,
                    });
                }
            }
        }
        keyframes.sort_by_key(|m| (m.sim_time, m.chunk, m.offset));
        deltas.sort_by_key(|m| (m.sim_time, m.chunk, m.offset));
        self.keyframes = keyframes;
        self.deltas = deltas;
        Ok(())
    }

    /// The last keyframe at or before `t` — §7.3 step 4, a binary search over an
    /// in-memory array.
    pub fn keyframe_at_or_before(&self, t: u64) -> Option<&MessageSlot> {
        let i = self.keyframes.partition_point(|m| m.sim_time <= t);
        if i == 0 {
            None
        } else {
            self.keyframes.get(i - 1)
        }
    }

    /// The recorded snapshot interval, `None` if the recording holds no keyframe.
    pub fn snapshot_span(&self) -> Option<(u64, u64)> {
        let first = self.keyframes.first()?.sim_time;
        let last = self
            .deltas
            .last()
            .map(|d| d.sim_time)
            .unwrap_or(0)
            .max(self.keyframes.last()?.sim_time);
        Some((first, last))
    }

    /// Reads and decompresses one chunk, handing every message in it to `f` in order.
    ///
    /// This is §7.3 steps 5–7: exactly one chunk's bytes leave the disk, and exactly one
    /// chunk is decompressed.
    ///
    /// # What the return value says
    ///
    /// [`ChunkIntegrity`], because the chunk's checksum can be switched off from the wire
    /// and a caller that is about to call the file verified needs to know whether anything
    /// was checked. See [`ChunkIntegrity`] for the policy and why it is this one.
    ///
    /// # Errors
    /// [`RecordError::Truncated`] if the chunk range is outside the file,
    /// [`RecordError::Malformed`] if the record at the chunk offset is not a chunk or its
    /// header disagrees with the chunk index, [`RecordError::WireOverflow`] if a length in
    /// the chunk header does not fit the arithmetic that would use it, and
    /// [`RecordError::Mcap`] if decompression, the chunk CRC or record parsing fails.
    /// Never a panic, an abort or an allocation sized by a number that came off the wire
    /// unchecked — see the module note.
    pub fn for_each_message_in_chunk(
        &self,
        src: &mut impl Source,
        chunk: usize,
        mut f: impl FnMut(&records::MessageHeader, &[u8], u16) -> Result<()>,
    ) -> Result<ChunkIntegrity> {
        let span = self.chunks.get(chunk).ok_or_else(|| {
            RecordError::malformed("mcap chunk index", format!("no chunk {chunk}"))
        })?;
        let len = usize::try_from(span.record_length).unwrap_or(0);
        if len <= RECORD_PREFIX {
            return Err(RecordError::malformed(
                "mcap chunk",
                format!("chunk {chunk} is {len} bytes, shorter than a record prefix"),
            ));
        }
        let bytes = src.read_at(span.file_offset, len)?;
        if bytes[0] != op::CHUNK {
            return Err(RecordError::malformed(
                "mcap chunk",
                format!(
                    "the record at {} has opcode {:#04x}, not a chunk",
                    span.file_offset, bytes[0]
                ),
            ));
        }
        let (limit, integrity) =
            self.validated_chunk_header(chunk, span, &bytes[RECORD_PREFIX..])?;
        let mut reader = mcap::sans_io::LinearReader::new_with_options(
            mcap::sans_io::LinearReaderOptions::default()
                // The buffer is one chunk record, not a file: no magic at either end.
                .with_skip_start_magic(true)
                .with_skip_end_magic(true)
                // Verify the chunk's CRC before parsing anything out of it, so a corrupt
                // payload cannot be read as a record length.
                .with_prevalidate_chunk_crcs(true)
                // No record inside a chunk is longer than the chunk's uncompressed size,
                // which the cross-check above bounded.
                .with_record_length_limit(limit),
        );
        let mut pos = 0usize;
        while let Some(event) = reader.next_event() {
            match event? {
                mcap::sans_io::LinearReadEvent::ReadRequest(need) => {
                    let take = need.min(bytes.len() - pos);
                    if take > 0 {
                        reader.insert(need)[..take].copy_from_slice(&bytes[pos..pos + take]);
                    }
                    reader.notify_read(take);
                    pos += take;
                }
                mcap::sans_io::LinearReadEvent::Record { data, opcode } => {
                    if opcode == op::MESSAGE {
                        if let records::Record::Message { header, data } =
                            mcap::read::parse_record(opcode, data)?
                        {
                            f(&header, &data, header.channel_id)?;
                        }
                    }
                }
            }
        }
        Ok(integrity)
    }

    /// Cross-checks a chunk record's own header against the summary's chunk index and
    /// against [`MAX_CHUNK_UNCOMPRESSED_BYTES`], and returns the record-length cap to read
    /// it under.
    ///
    /// The index is the authoritative second copy: it is written from the same numbers, in
    /// a different part of the file, so a bit flip in either one is caught by the
    /// disagreement. Everything a decompressor would size an allocation from is checked
    /// here, before it is handed over.
    ///
    /// Returns the record-length cap and whether the chunk carries a checksum at all — the
    /// second is [`ChunkIntegrity`]'s reason for existing, and it is read here because this
    /// is the one place the chunk's own header is decoded.
    fn validated_chunk_header(
        &self,
        chunk: usize,
        span: &ChunkSpan,
        body: &[u8],
    ) -> Result<(usize, ChunkIntegrity)> {
        let header = match mcap::read::parse_record(op::CHUNK, body)? {
            records::Record::Chunk { header, .. } => header,
            _ => {
                return Err(RecordError::malformed(
                    "mcap chunk",
                    "the chunk record did not parse as a chunk",
                ));
            }
        };
        let disagreement = |what: &str, found: String, index: String| {
            RecordError::malformed(
                "mcap chunk",
                format!(
                    "chunk {chunk} at {} declares {what} = {found} but the chunk index says                      {index}; one of the two is corrupt",
                    span.file_offset
                ),
            )
        };
        if header.uncompressed_size != span.uncompressed_size {
            return Err(disagreement(
                "uncompressed_size",
                header.uncompressed_size.to_string(),
                span.uncompressed_size.to_string(),
            ));
        }
        if header.compression != span.compression {
            return Err(disagreement(
                "compression",
                format!("{:?}", header.compression),
                format!("{:?}", span.compression),
            ));
        }
        if header.message_start_time != span.message_start_time
            || header.message_end_time != span.message_end_time
        {
            return Err(disagreement(
                "its message time span",
                format!(
                    "[{}, {}]",
                    header.message_start_time, header.message_end_time
                ),
                format!("[{}, {}]", span.message_start_time, span.message_end_time),
            ));
        }
        if header.uncompressed_size > MAX_CHUNK_UNCOMPRESSED_BYTES {
            return Err(RecordError::malformed(
                "mcap chunk",
                format!(
                    "chunk {chunk} declares {} uncompressed bytes, past the {MAX_CHUNK_UNCOMPRESSED_BYTES}-byte                      ceiling this reader will allocate for",
                    header.uncompressed_size
                ),
            ));
        }
        // The chunk record is exactly its prefix, its header and its compressed data:
        // `opcode || length || (8 start + 8 end + 8 uncompressed_size + 4 crc + 4
        // compression_len + compression + 8 compressed_size) || data`. MCAP's own reader
        // treats any excess as padding and then waits for it, so a `compressed_size` a bit
        // flip made smaller would have it wait for bytes that are not coming — with an
        // empty input and a non-empty output buffer, which is a spin, not an error.
        // Requiring the record to add up refuses that here instead.
        const CHUNK_HEADER_FIXED: u64 = 8 + 8 + 8 + 4 + 4 + 8;
        // `compressed_size` is a wire `u64`. `mcap::read::parse_record` already refuses a
        // chunk whose `compressed_size` exceeds the bytes present, so today this sum is
        // bounded by the record length — but that is the MCAP crate's invariant, not this
        // function's, and an unchecked `+` here would have exactly the shape that made the
        // chunk-index check useless: a wrap could land on `span.record_length` and pass.
        let declared = (RECORD_PREFIX as u64)
            .checked_add(CHUNK_HEADER_FIXED)
            .and_then(|n| n.checked_add(header.compression.len() as u64))
            .and_then(|n| n.checked_add(header.compressed_size))
            .ok_or_else(|| {
                RecordError::wire_overflow(
                    "mcap chunk",
                    format!(
                        "chunk {chunk} declares {} compressed bytes, which with its header does not fit                          in 64 bits",
                        header.compressed_size
                    ),
                )
            })?;
        if declared != span.record_length {
            return Err(RecordError::malformed(
                "mcap chunk",
                format!(
                    "chunk {chunk} declares a {}-byte header and {} compressed bytes,                      {declared} in all, inside a {}-byte record",
                    CHUNK_HEADER_FIXED + header.compression.len() as u64,
                    header.compressed_size,
                    span.record_length
                ),
            ));
        }
        // §4 of the MCAP specification: "A zero value indicates that CRC validation should
        // not be performed." `LinearReader` implements that literally — `prevalidate_chunk_crcs`
        // is skipped when the stored CRC is zero — so the four bytes at chunk-header offset
        // 24 are a switch the file can throw to turn this reader's only content check off.
        // The chunk is still read, because a zero CRC is legal and a conforming third-party
        // file must not be refused over it, but the caller is told, because reporting a file
        // as verified when nothing was checked is the failure that must not happen.
        let integrity = if header.uncompressed_crc == 0 {
            ChunkIntegrity::NotChecked
        } else {
            ChunkIntegrity::Verified
        };
        // `usize::try_from` cannot fail after the ceiling above on any target this crate
        // builds for; `unwrap_or` keeps the promise on one where it could.
        Ok((
            usize::try_from(header.uncompressed_size).unwrap_or(usize::MAX),
            integrity,
        ))
    }

    /// Reads a metadata record by name, returning its key/value map.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if there is no such metadata record or the record at its
    /// offset is not one.
    pub fn read_metadata(
        &self,
        src: &mut impl Source,
        name: &str,
    ) -> Result<BTreeMap<String, String>> {
        let (offset, length) = *self.metadata.get(name).ok_or_else(|| {
            RecordError::malformed(
                "mcap metadata",
                format!("no metadata record named {name:?}"),
            )
        })?;
        let bytes = src.read_at(offset, usize::try_from(length).unwrap_or(0))?;
        if bytes.len() <= RECORD_PREFIX || bytes[0] != op::METADATA {
            return Err(RecordError::malformed(
                "mcap metadata",
                format!("the record at {offset} is not a metadata record"),
            ));
        }
        match mcap::read::parse_record(op::METADATA, &bytes[RECORD_PREFIX..])? {
            records::Record::Metadata(m) => Ok(m.metadata.into_iter().collect()),
            _ => Err(RecordError::malformed(
                "mcap metadata",
                "the metadata record did not parse as one",
            )),
        }
    }

    /// Reads an attachment's bytes by name.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if there is no such attachment.
    pub fn read_attachment(&self, src: &mut impl Source, name: &str) -> Result<Vec<u8>> {
        let (offset, length) = *self.attachments.get(name).ok_or_else(|| {
            RecordError::malformed("mcap attachment", format!("no attachment named {name:?}"))
        })?;
        let bytes = src.read_at(offset, usize::try_from(length).unwrap_or(0))?;
        if bytes.len() <= RECORD_PREFIX || bytes[0] != op::ATTACHMENT {
            return Err(RecordError::malformed(
                "mcap attachment",
                format!("the record at {offset} is not an attachment"),
            ));
        }
        match mcap::read::parse_record(op::ATTACHMENT, &bytes[RECORD_PREFIX..])? {
            records::Record::Attachment { data, .. } => Ok(data.into_owned()),
            _ => Err(RecordError::malformed(
                "mcap attachment",
                "the attachment record did not parse as one",
            )),
        }
    }
}

/// Splits a byte range into MCAP records: `opcode || length || body`, repeated.
fn records_in<'a>(buf: &'a [u8], what: &'static str) -> Result<Vec<(u8, &'a [u8])>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < buf.len() {
        if p + RECORD_PREFIX > buf.len() {
            return Err(RecordError::Truncated {
                what,
                at: p,
                need: RECORD_PREFIX,
                have: buf.len() - p,
            });
        }
        let op_code = buf[p];
        let len = u64::from_le_bytes(buf[p + 1..p + 9].try_into().unwrap_or([0; 8]));
        let len = usize::try_from(len).map_err(|_| RecordError::Truncated {
            what,
            at: p,
            need: usize::MAX,
            have: buf.len() - p,
        })?;
        let start = p + RECORD_PREFIX;
        let end = start.checked_add(len).ok_or(RecordError::Truncated {
            what,
            at: start,
            need: len,
            have: buf.len() - start,
        })?;
        if end > buf.len() {
            return Err(RecordError::Truncated {
                what,
                at: start,
                need: len,
                have: buf.len() - start,
            });
        }
        out.push((op_code, &buf[start..end]));
        p = end;
    }
    Ok(out)
}
