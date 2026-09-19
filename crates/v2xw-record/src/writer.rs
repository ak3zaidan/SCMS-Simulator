//! The recorder: MCAP with zstd chunks, one channel per family, each self-describing
//! (§7.1, ADR 0008 decision 1).
//!
//! # What the recorder does not do
//!
//! It does not re-encode a snapshot frame. [`RecordingWriter::write_frame`] stores the
//! bytes it is handed, with only the transport flag bits cleared (§7.2 item 3), and that
//! is the whole of the storage side of the byte-identity guarantee. Everything else this
//! module does — topics, schemas, channel metadata, chunk boundaries, the manifest,
//! attachments — is about making the file navigable and self-describing, and none of it
//! touches a frame's bytes.
//!
//! # Two encodings, not unified (build decision D11 item 5)
//!
//! * [`RecordingWriter::write_frame`] puts VWP frames on `vwp/…` topics, message encoding
//!   `vwp1`, schema `vwp.v1.<Type>` whose data is the layout tables themselves.
//! * [`RecordingWriter::write_record`] puts serde [`v2xw_core::OwnedRecord`]s on
//!   `record/…` topics, message encoding `json`, schema `jsonschema`. These are the rows
//!   the Parquet, Arrow IPC and JSONL exporters read.
//!
//! A topic never carries both, and the recorder refuses an attempt to make it.
//!
//! # Determinism of the file, not just of the frames
//!
//! zstd's multithreaded encoder does not produce identical bytes for identical input, so
//! [`RecordingOptions`] sets `compression_threads` to zero. Without that, two runs of the
//! same scenario produce files whose frames are identical and whose SHA-256 is not, which
//! would quietly break the run digest of 02-architecture §6.5.
//!
//! # Chunk boundaries fall on keyframes, and `chunk_target_bytes` is a bound
//!
//! §7.1 requires that "a chunk MUST NOT start in the middle of a GOP's keyframe", and
//! §7.3 step 7 depends on a GOP spanning at most two chunks. The recorder therefore takes
//! the chunk boundary into its own hands: the container's own size-triggered split is
//! switched off (`chunk_size(None)`), and the recorder closes the current chunk *before* a
//! keyframe once the chunk has reached its target size. While a GOP fits the target — the
//! case every realistic cadence is in — every chunk begins at a keyframe and no GOP is
//! ever split, so a seek reads exactly one chunk.
//!
//! That rule alone does not bound a chunk, and an earlier version of this module wrongly
//! implied it did. A chunk held *at least one whole GOP*, so its size grew with
//! `keyframe_period / mobility_step`: at 400 actors and a 100 ms step a 1 s keyframe
//! period gives a 4.0 MiB chunk and a 600 s one gives roughly 48 MiB, twelve times the
//! size §7.4's latency budget is written against — silently, because nothing measured it.
//! [`RecordingWriter`] therefore also ends a chunk *between* deltas once the open GOP has
//! itself passed the target, which §7.1 permits (it forbids splitting a keyframe record,
//! not a GOP) and which §7.3 step 7 explicitly contemplates. The bound is then
//! `chunk_target_bytes` plus one whole GOP — and a GOP that does not fit the target is
//! itself split, so the bound is twice the target plus one message *whatever the keyframe
//! period*, rather than growing without limit with it. Where the GOP is the thing that
//! does not fit, a chunk opens at its keyframe and the bound is the target plus one
//! message. [`RecordingSummary::largest_chunk_bytes`] reports what was actually reached,
//! and [`RecordingSummary::largest_gop_bytes`] says which of the two cases a run was in.
//!
//! A recording with no snapshot channel at all — a dataset export of event records, say —
//! is not inside a GOP anywhere, so a boundary is legal at any message and the same
//! target applies.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::Path;

use v2xw_core::time::SimTime;
use v2xw_core::{ErasedRecord, OwnedRecord, Visibility};

use crate::channels::{self, ChannelSpec};
use crate::encoder::Cadence;
use crate::error::{RecordError, Result};
use crate::profile::Profile;
use crate::wire::{CANONICAL_FLAG_MASK, Frame, MsgType};

/// The topic carrying the recorded `Hello` (§7.1).
pub const TOPIC_HELLO: &str = "vwp/hello";
/// The topic carrying `Keyframe` frames (§7.1).
pub const TOPIC_KEYFRAME: &str = "vwp/keyframe";
/// The topic carrying `Delta` frames (§7.1).
pub const TOPIC_DELTA: &str = "vwp/delta";
/// The topic carrying `Telemetry` frames (§7.1).
pub const TOPIC_TELEMETRY: &str = "vwp/telemetry";
/// The topic carrying `MetricSample` frames (§7.1).
pub const TOPIC_METRIC: &str = "vwp/metric";
/// The topic carrying `Provenance` frames (§7.1).
pub const TOPIC_PROVENANCE: &str = "vwp/provenance";

/// The MCAP message encoding of a topic carrying whole VWP frames (§7.1).
pub const ENCODING_VWP: &str = "vwp1";
/// The MCAP message encoding of a topic carrying serde records (D11 item 5).
pub const ENCODING_JSON: &str = "json";

/// The name of the metadata record holding the run manifest (§7.1).
pub const METADATA_MANIFEST: &str = "v2xw.manifest";

/// The MCAP profile string this crate writes.
pub const MCAP_PROFILE: &str = "v2xw";

/// How the recorder is configured for a run.
#[derive(Debug, Clone)]
pub struct RecordingOptions {
    /// The snapshot cadence, from the scenario.
    pub cadence: Cadence,
    /// Which stream is being recorded.
    pub profile: Profile,
    /// The target uncompressed chunk size; §7.1 says 4 MiB.
    ///
    /// It is an upper bound up to one message: the recorder closes a chunk before the
    /// message that would take it past this, at a keyframe where it can and between
    /// deltas where the GOP alone is larger than the target. See the module note.
    pub chunk_target_bytes: u64,
    /// The zstd level; §2.6 decides 3.
    pub compression_level: u32,
    /// The library string written into the MCAP header.
    pub library: String,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        RecordingOptions {
            cadence: Cadence::DEFAULT,
            profile: Profile::Full,
            chunk_target_bytes: 4 * 1024 * 1024,
            compression_level: 3,
            library: concat!("v2xw-record ", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }
}

/// What a finished recording holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordingSummary {
    /// Messages written, both encodings together.
    pub message_count: u64,
    /// VWP frames written.
    pub frame_count: u64,
    /// serde records written.
    pub record_count: u64,
    /// Chunks the writer closed.
    ///
    /// Every chunk, including the one an attachment or a metadata record closes: `attach`
    /// and `write_manifest` end the open chunk inside the container, and counting the
    /// reset without counting the chunk left this one short by one for any recording that
    /// ended with either.
    pub chunk_count: u64,
    /// The largest number of uncompressed message bytes the writer put in one chunk.
    ///
    /// Reported rather than assumed: `chunk_target_bytes` bounds a chunk only up to the
    /// message that crosses it, and this is the number that says by how much. It counts
    /// the stored payloads, not the container's record framing or its compressed size.
    pub largest_chunk_bytes: u64,
    /// The largest number of uncompressed message bytes one GOP contributed.
    ///
    /// A GOP larger than `chunk_target_bytes` is the condition under which a chunk is
    /// split between deltas (see the module note), so this is what a caller checks to
    /// find out whether a cadence left §7.4's budget.
    pub largest_gop_bytes: u64,
}

/// Writes a recording.
pub struct RecordingWriter<W: Write + Seek> {
    mcap: mcap::Writer<W>,
    opts: RecordingOptions,
    channels: BTreeMap<String, u16>,
    topic_encoding: BTreeMap<String, &'static str>,
    bytes_since_flush: u64,
    bytes_since_keyframe: u64,
    keyframes_since_flush: u64,
    summary: RecordingSummary,
    last_log_time: u64,
}

impl<W: Write + Seek> std::fmt::Debug for RecordingWriter<W> {
    /// Shows what has been written, not the container's internals: an `mcap::Writer` has
    /// no useful rendering and printing one would invite treating it as inspectable state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingWriter")
            .field("profile", &self.opts.profile)
            .field("cadence", &self.opts.cadence)
            .field("summary", &self.summary)
            .field("channels", &self.channels.len())
            .finish_non_exhaustive()
    }
}

impl RecordingWriter<BufWriter<File>> {
    /// Creates a recording at `path`.
    ///
    /// # Errors
    /// [`RecordError::Io`] if the file cannot be created, [`RecordError::Mcap`] if the
    /// MCAP header cannot be written.
    pub fn create(path: impl AsRef<Path>, opts: RecordingOptions) -> Result<Self> {
        let path = path.as_ref();
        let file = File::create(path).map_err(|e| RecordError::io(path, e))?;
        Self::new(BufWriter::new(file), opts)
    }
}

impl<W: Write + Seek> RecordingWriter<W> {
    /// Creates a recording on an arbitrary sink — a `Cursor<Vec<u8>>` in tests.
    ///
    /// # Errors
    /// [`RecordError::Mcap`] if the MCAP header cannot be written.
    pub fn new(sink: W, opts: RecordingOptions) -> Result<Self> {
        let mcap = mcap::WriteOptions::new()
            .compression(Some(mcap::Compression::Zstd))
            .compression_level(opts.compression_level)
            // Deterministic bytes: see the module note.
            .compression_threads(0)
            // The recorder decides where chunks end; see the module note on keyframes.
            .chunk_size(None)
            .profile(MCAP_PROFILE)
            .library(opts.library.clone())
            .create(sink)?;
        Ok(RecordingWriter {
            mcap,
            opts,
            channels: BTreeMap::new(),
            topic_encoding: BTreeMap::new(),
            bytes_since_flush: 0,
            bytes_since_keyframe: 0,
            keyframes_since_flush: 0,
            summary: RecordingSummary::default(),
            last_log_time: 0,
        })
    }

    /// The options this recording was opened with.
    pub fn options(&self) -> &RecordingOptions {
        &self.opts
    }

    /// What has been written so far.
    pub fn summary(&self) -> RecordingSummary {
        self.summary
    }

    /// Stores one VWP frame verbatim (§7.1, §7.2 item 2).
    ///
    /// The only change made to the bytes is clearing [`crate::wire::TRANSPORT_FLAG_MASK`]
    /// in the header's flags, which §7.2 item 3 requires and which is what makes the
    /// stored frame comparable to a live one.
    ///
    /// A mixed-channel `Event` batch is split into one message per channel, because
    /// §7.1 requires per-channel message indexes to work; a single-channel batch is
    /// stored untouched. That split is the one case in which a stored frame is not
    /// byte-identical to the frame that went over the wire, and the specification asks
    /// for it explicitly.
    ///
    /// A `msg_type` this build does not know is **stored**, on `vwp/unknown.<id>`, not
    /// refused: §8.4 makes a new message type an additive change and §8.6 has the replay
    /// reader accept a higher minor "ignoring what it does not know" (conformance F6, N1).
    /// A recorder that refused one would make a v1.1 stream unrecordable by a v1 build,
    /// which is the opposite of what forward compatibility is for. Its body is stored
    /// verbatim and never decoded, because this build has no layout for it — and for the
    /// same reason the §5.3 profile-flag check and the D9 grid scan are not applied to it:
    /// neither can be evaluated without knowing what the frame is. That is the honest cost
    /// of carrying a message this build cannot read, and it is why
    /// [`crate::reader::VerifyReport::unknown_frames`] counts them.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] for a frame the recording does not carry (`Error`,
    /// `Bye`, `WorldChunk`), [`RecordError::OffGrid`] if a float in it is off its declared
    /// grid (D9), or [`RecordError::Mcap`] from the container.
    pub fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        let header = frame.header()?;
        let Some(kind) = header.kind() else {
            // §8.4/§8.6: ignore what this build does not know rather than refusing it.
            return self.store(frame, &unknown_topic(header.msg_type), None);
        };
        match kind {
            MsgType::Error | MsgType::Bye => {
                return Err(RecordError::malformed(
                    "vwp frame",
                    format!(
                        "{} is a connection frame and is not part of the canonical stream (§0.1)",
                        kind.schema_name()
                    ),
                ));
            }
            MsgType::WorldChunk => {
                return Err(RecordError::malformed(
                    "vwp frame",
                    "a recording carries the world as the `world.vwb` attachment, not as WorldChunk frames (§7.1)",
                ));
            }
            _ => {}
        }
        // §5.3: the server sets FLAG_NODE_ONLY on every *canonical* frame of a node
        // stream. `Hello` is a connection frame and carries the profile in `hello_flags`
        // instead, so it is exempt.
        if self.opts.profile.is_node_only()
            && kind.is_canonical()
            && header.flags & crate::wire::FLAG_NODE_ONLY == 0
        {
            return Err(RecordError::malformed(
                "vwp frame",
                "a node-profile recording accepts only canonical frames carrying FLAG_NODE_ONLY (§5.3)",
            ));
        }
        crate::grid::scan_frame(frame)?;

        if kind == MsgType::Event {
            let body = crate::wire::event::EventBody::decode(frame.body())?;
            let ids = body.channel_ids();
            if ids.len() > 1 {
                for id in ids {
                    if let Some(split) = body.only_channel(id) {
                        let split_frame = split.to_frame(header.seq, header.flags)?;
                        self.store(&split_frame, &event_topic(id), Some(kind))?;
                    }
                }
                return Ok(());
            }
            let Some(id) = ids.first().copied() else {
                return Err(RecordError::malformed(
                    "vwp Event",
                    "an Event batch with no entries names no channel, so there is no topic to \
                     record it on; a producer with nothing to say sends no frame",
                ));
            };
            return self.store(frame, &event_topic(id), Some(kind));
        }

        let topic = match kind {
            MsgType::Hello => TOPIC_HELLO,
            MsgType::Keyframe => TOPIC_KEYFRAME,
            MsgType::Delta => TOPIC_DELTA,
            MsgType::Telemetry => TOPIC_TELEMETRY,
            MsgType::MetricSample => TOPIC_METRIC,
            MsgType::Provenance => TOPIC_PROVENANCE,
            MsgType::Event | MsgType::Error | MsgType::Bye | MsgType::WorldChunk => unreachable!(),
        };
        self.store(frame, topic, Some(kind))
    }

    fn store(&mut self, frame: &Frame, topic: &str, kind: Option<MsgType>) -> Result<()> {
        let header = frame.header()?;
        // A type this build does not know carries no layout, so there is no prefix to read
        // a time out of; it is filed at the time of the frame before it, which keeps
        // `log_time` monotonic and the MCAP time index usable (§8.4).
        let log_time = match frame.sim_time() {
            Ok(t) => t,
            Err(_) if kind.is_none() => self.last_log_time,
            Err(e) => return Err(e),
        };
        let is_keyframe = kind == Some(MsgType::Keyframe);
        self.end_chunk_if_over_target(is_keyframe)?;
        if is_keyframe {
            self.keyframes_since_flush += 1;
            self.bytes_since_keyframe = 0;
        }
        let canonical = frame.with_flags(header.flags & CANONICAL_FLAG_MASK);
        let channel_id = self.frame_channel(topic, kind, header.msg_type)?;
        self.mcap.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id,
                // MCAP's `sequence` is 32 bits; the canonical 64-bit `seq` is in the
                // frame header, which is the authoritative copy (§7.1 maps `sequence` to
                // `seq`, and 4·10⁹ frames is 13 years of 10 Hz deltas).
                sequence: header.seq as u32,
                log_time,
                publish_time: log_time,
            },
            canonical.as_bytes(),
        )?;
        self.account(canonical.as_bytes().len() as u64);
        self.summary.message_count += 1;
        self.summary.frame_count += 1;
        self.last_log_time = self.last_log_time.max(log_time);
        Ok(())
    }

    /// Writes one serde record (build decision D11 item 5).
    ///
    /// Build decision D9 applies here exactly as it does to a frame: "no floating-point
    /// value reaches a **recorded**, exported or digested artefact in raw IEEE-754 form".
    /// The record's JSON is scanned against the declared grid of every field it carries,
    /// nested objects and arrays included, and an off-grid value is refused with
    /// [`RecordError::OffGrid`] — the same gate [`RecordingWriter::write_frame`] applies
    /// to a frame. Without it the exporters' quantisation hid the problem from
    /// `export::scan` while the recording itself, and [`crate::reader::Reader::content_digest`]
    /// over it, carried the raw digits.
    ///
    /// # Errors
    /// [`RecordError::UnknownChannel`] for a channel outside 03-interfaces §14,
    /// [`RecordError::VisibilityDenied`] for a ground-truth record on a node channel or
    /// any ground-truth record in a `node`-profile recording, [`RecordError::OffGrid`] for
    /// a float that is off its declared grid (D9), [`RecordError::Json`] if the record is
    /// not JSON, or [`RecordError::Mcap`].
    pub fn write_record(&mut self, at: SimTime, record: &OwnedRecord) -> Result<()> {
        let spec = channels::by_name(record.channel)
            .ok_or_else(|| RecordError::UnknownChannel(record.channel.to_string()))?;
        if spec.visibility.allowed_on_node_channel() && record.visibility.is_gt_tainted() {
            return Err(RecordError::VisibilityDenied {
                channel: record.channel.to_string(),
                visibility: record.visibility,
            });
        }
        if self.opts.profile.is_node_only() && record.visibility.is_gt_tainted() {
            return Err(RecordError::VisibilityDenied {
                channel: record.channel.to_string(),
                visibility: record.visibility,
            });
        }
        crate::grid::scan_record(record.channel, &record.json)?;
        let topic = spec.record_topic();
        let channel_id = self.record_channel(spec, &topic)?;
        self.end_chunk_if_over_target(false)?;
        self.mcap.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id,
                sequence: 0,
                log_time: at,
                publish_time: at,
            },
            &record.json,
        )?;
        self.account(record.json.len() as u64);
        self.summary.message_count += 1;
        self.summary.record_count += 1;
        self.last_log_time = self.last_log_time.max(at);
        Ok(())
    }

    /// Writes a typed record straight from a plug-in's [`ErasedRecord`].
    ///
    /// # Errors
    /// Whatever [`RecordingWriter::write_record`] returns, plus
    /// [`RecordError::Json`] if the record will not serialise.
    pub fn write_erased(&mut self, at: SimTime, record: &dyn ErasedRecord) -> Result<()> {
        let owned = record
            .to_owned_record()
            .map_err(|e| RecordError::malformed("record", e.to_string()))?;
        self.write_record(at, &owned)
    }

    /// Writes the run manifest and the format version as the `v2xw.manifest` metadata
    /// record (§7.1, §8.6).
    ///
    /// # Errors
    /// [`RecordError::Mcap`] from the container.
    pub fn write_manifest(&mut self, manifest_json: &str) -> Result<()> {
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "vwp_version_major".to_string(),
            crate::wire::VERSION_MAJOR.to_string(),
        );
        metadata.insert(
            "vwp_version_minor".to_string(),
            crate::wire::VERSION_MINOR.to_string(),
        );
        metadata.insert(
            "keyframe_period_ns".to_string(),
            self.opts.cadence.keyframe_period.as_nanos().to_string(),
        );
        metadata.insert(
            "mobility_step_ns".to_string(),
            self.opts.cadence.mobility_step.as_nanos().to_string(),
        );
        metadata.insert(
            "profile".to_string(),
            match self.opts.profile {
                Profile::Full => "full".to_string(),
                Profile::NodeOnly => "node".to_string(),
            },
        );
        metadata.insert("json".to_string(), manifest_json.to_string());
        // A metadata record closes the current chunk before it is written.
        self.mcap.write_metadata(&mcap::records::Metadata {
            name: METADATA_MANIFEST.to_string(),
            metadata: metadata.into_iter().collect(),
        })?;
        self.chunk_closed_by_container();
        Ok(())
    }

    /// Attaches a file — §7.1's `scenario.yaml` and `world.vwb`, stored as the exact
    /// bytes the run used.
    ///
    /// # Errors
    /// [`RecordError::Mcap`] from the container.
    pub fn attach(&mut self, name: &str, media_type: &str, data: &[u8]) -> Result<()> {
        self.mcap.attach(&mcap::Attachment {
            log_time: self.last_log_time,
            create_time: self.last_log_time,
            name: name.to_string(),
            media_type: media_type.to_string(),
            data: std::borrow::Cow::Borrowed(data),
        })?;
        self.chunk_closed_by_container();
        Ok(())
    }

    /// Finishes the recording: closes the last chunk and writes the summary section.
    ///
    /// A recording that is not finished has no summary and cannot be seeked
    /// ([`RecordError::NoSummary`]), which is what an interrupted run looks like.
    ///
    /// # Errors
    /// [`RecordError::Mcap`] from the container.
    pub fn finish(mut self) -> Result<RecordingSummary> {
        self.mcap.finish()?;
        if self.bytes_since_flush > 0 {
            self.summary.chunk_count += 1;
        }
        Ok(self.summary)
    }

    /// Records the bytes one stored message added to the open chunk and to the open GOP.
    fn account(&mut self, bytes: u64) {
        self.bytes_since_flush += bytes;
        self.bytes_since_keyframe += bytes;
        self.summary.largest_chunk_bytes =
            self.summary.largest_chunk_bytes.max(self.bytes_since_flush);
        self.summary.largest_gop_bytes = self
            .summary
            .largest_gop_bytes
            .max(self.bytes_since_keyframe);
    }

    /// Closes the current chunk and starts counting a new one.
    fn end_chunk(&mut self) -> Result<()> {
        self.mcap.flush()?;
        self.bytes_since_flush = 0;
        self.keyframes_since_flush = 0;
        self.summary.chunk_count += 1;
        Ok(())
    }

    /// An attachment or a metadata record finishes the open chunk inside the container.
    ///
    /// `bytes_since_keyframe` is deliberately *not* reset: the GOP that was open is still
    /// open, and forgetting that would let the next chunk swallow the rest of it whole.
    fn chunk_closed_by_container(&mut self) {
        if self.bytes_since_flush > 0 {
            self.summary.chunk_count += 1;
        }
        self.bytes_since_flush = 0;
        self.keyframes_since_flush = 0;
    }

    /// Ends the open chunk if the next message would take it past the target, and if a
    /// boundary is legal here.
    ///
    /// Legal here means: immediately before a keyframe (§7.1's own rule), or anywhere in a
    /// chunk that is not inside a GOP, or inside a GOP that is itself larger than the
    /// target — because such a GOP can never fit a target-sized chunk and §7.1 forbids
    /// splitting a keyframe *record*, not a GOP (§7.3 step 7 reads the GOP's chunks). See
    /// the module note; without the last case `chunk_target_bytes` is not a bound at all.
    fn end_chunk_if_over_target(&mut self, at_keyframe: bool) -> Result<()> {
        let target = self.opts.chunk_target_bytes;
        // The GOP that is ending here did not fit the target, so the next one will not
        // either: open its chunk at its keyframe rather than carrying the tail of this one
        // into it. Without this the next chunk runs to the target *plus* what was already
        // in it, and the bound is twice the target rather than the target.
        if at_keyframe && self.bytes_since_flush > 0 && self.bytes_since_keyframe >= target {
            return self.end_chunk();
        }
        if self.bytes_since_flush < target {
            return Ok(());
        }
        let inside_gop = self.keyframes_since_flush > 0 && self.bytes_since_keyframe < target;
        if at_keyframe || !inside_gop {
            self.end_chunk()?;
        }
        Ok(())
    }

    fn frame_channel(&mut self, topic: &str, kind: Option<MsgType>, msg_type: u16) -> Result<u16> {
        if let Some(id) = self.channels.get(topic) {
            self.check_encoding(topic, ENCODING_VWP)?;
            return Ok(*id);
        }
        let (schema_name, layout) = match kind {
            Some(kind) => (
                kind.schema_name().to_string(),
                crate::schema::layout_for(kind),
            ),
            None => (
                format!("vwp.v1.Unknown.{msg_type:#06x}"),
                crate::schema::layout_for_unknown(msg_type),
            ),
        };
        let schema_id = self
            .mcap
            .add_schema(&schema_name, ENCODING_VWP, layout.as_bytes())?;
        let spec = frame_channel_spec(topic);
        let mut metadata = BTreeMap::new();
        metadata.insert("encoding_kind".to_string(), "vwp1".to_string());
        metadata.insert("vwp_msg_type".to_string(), format!("{msg_type:#06x}"));
        if kind.is_none() {
            metadata.insert("vwp_unknown_type".to_string(), "true".to_string());
        }
        if let Some(spec) = spec {
            metadata.insert("channel".to_string(), spec.name.to_string());
            metadata.insert("visibility".to_string(), spec.visibility.to_string());
            metadata.insert(
                "ground_truth".to_string(),
                self.carries_gt(spec).to_string(),
            );
            if let Some(id) = spec.wire_id {
                metadata.insert("vwp_channel_id".to_string(), id.to_string());
            }
        } else {
            metadata.insert("visibility".to_string(), Visibility::Meta.to_string());
            metadata.insert("ground_truth".to_string(), "false".to_string());
        }
        let id = self.mcap.add_channel(
            schema_id,
            topic,
            ENCODING_VWP,
            &metadata.into_iter().collect(),
        )?;
        self.channels.insert(topic.to_string(), id);
        self.topic_encoding.insert(topic.to_string(), ENCODING_VWP);
        Ok(id)
    }

    fn record_channel(&mut self, spec: &ChannelSpec, topic: &str) -> Result<u16> {
        if let Some(id) = self.channels.get(topic) {
            self.check_encoding(topic, ENCODING_JSON)?;
            return Ok(*id);
        }
        let schema = crate::schema::record_schema_json(spec);
        let schema_id = self.mcap.add_schema(
            &format!("v2xw.record.{}", spec.name),
            "jsonschema",
            schema.as_bytes(),
        )?;
        let mut metadata = BTreeMap::new();
        metadata.insert("encoding_kind".to_string(), "record".to_string());
        metadata.insert("channel".to_string(), spec.name.to_string());
        metadata.insert("visibility".to_string(), spec.visibility.to_string());
        metadata.insert(
            "ground_truth".to_string(),
            self.carries_gt(spec).to_string(),
        );
        let id = self.mcap.add_channel(
            schema_id,
            topic,
            ENCODING_JSON,
            &metadata.into_iter().collect(),
        )?;
        self.channels.insert(topic.to_string(), id);
        self.topic_encoding.insert(topic.to_string(), ENCODING_JSON);
        Ok(id)
    }

    /// Whether a channel of this recording actually carries ground truth.
    ///
    /// The channel's *declared* visibility does not change with the profile — `phy.rx` is
    /// a mixed channel wherever it appears, and saying otherwise would rename it. What
    /// changes is whether this file's copy of it holds any: under the `node` profile the
    /// producer blanked every ground-truth field before serialising (§5.3), so nothing in
    /// the file does. The tag is what a leakage linter reads, so it must describe the
    /// file rather than the schema.
    fn carries_gt(&self, spec: &ChannelSpec) -> bool {
        spec.is_gt_tainted() && !self.opts.profile.is_node_only()
    }

    fn check_encoding(&self, topic: &str, want: &'static str) -> Result<()> {
        match self.topic_encoding.get(topic) {
            Some(have) if *have != want => Err(RecordError::malformed(
                "recording",
                format!("topic {topic:?} already carries {have}, it cannot also carry {want}"),
            )),
            _ => Ok(()),
        }
    }
}

/// The `vwp/event/<channel-name>` topic for a wire channel id (§7.1).
///
/// A channel id this build does not know still gets a topic, named by its number, so a
/// plug-in channel is recorded rather than dropped (§3.6.2, §8.4).
pub fn event_topic(channel_id: u16) -> String {
    match channels::by_wire_id(channel_id) {
        Some(spec) => spec.event_topic(),
        None => format!("vwp/event/plugin.{channel_id}"),
    }
}

/// The `vwp/unknown.<id>` topic a message type this build does not know is stored on
/// (§8.4, conformance F6 and N1).
///
/// Named by its number for the same reason a plug-in event channel is: the frame is
/// carried rather than dropped, so a reader built for the minor version that defines it
/// finds it where it was.
pub fn unknown_topic(msg_type: u16) -> String {
    format!("vwp/unknown.{msg_type}")
}

/// The 03-interfaces §14 channel a VWP frame topic corresponds to, for its visibility tag.
fn frame_channel_spec(topic: &str) -> Option<&'static ChannelSpec> {
    match topic {
        TOPIC_KEYFRAME => channels::by_name("snapshot.keyframe"),
        TOPIC_DELTA => channels::by_name("snapshot.delta"),
        TOPIC_TELEMETRY => channels::by_name("node.telemetry"),
        TOPIC_METRIC => channels::by_name("metric.sample"),
        TOPIC_HELLO => channels::by_name("manifest"),
        _ => topic.strip_prefix("vwp/event/").and_then(channels::by_name),
    }
}
