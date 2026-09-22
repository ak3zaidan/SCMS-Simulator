//! The replay reader: the same stream the live engine produced, plus `verify` and `seek`.
//!
//! # The property this module exists to have
//!
//! §7.2's guarantee is that for every canonical frame the 24-byte header with
//! `flags &= CANONICAL_FLAG_MASK` and the entire body are byte-identical whether the
//! frame came from the live engine or from here. The implementation of that guarantee is
//! one line long — [`Reader::replay`] hands back the stored bytes — and everything else
//! in this file is in service of not breaking it. In particular there is no "decode the
//! state and re-emit it" path anywhere, because that is precisely what ADR 0008's
//! amendment dropped FlatBuffers to avoid.
//!
//! # Paged or resident
//!
//! [`Reader::open`] reads the file into memory; [`Reader::open_paged`] reads the footer,
//! the summary and the message indexes and then fetches only the chunks a seek needs
//! (§7.4's "read 1–2 chunks from disk"). The two share every algorithm through
//! [`crate::index::Source`].

use std::collections::BTreeMap;
use std::path::Path;

use v2xw_core::time::SimTime;

use crate::encoder::Cadence;
use crate::error::{RecordError, Result};
use crate::index::{ChannelMeta, ChunkIntegrity, FileSource, MemorySource, SeekIndex, Source};
use crate::profile::Profile;
use crate::wire::snapshot::{DeltaBody, KeyframeBody};
use crate::wire::{
    CANONICAL_FLAG_MASK, FLAG_NODE_ONLY, FLAG_RESYNC, FLAG_SEEK_RESULT, Frame, MsgType,
    TRANSPORT_FLAG_MASK,
};
use crate::writer::{ENCODING_JSON, ENCODING_VWP, METADATA_MANIFEST, TOPIC_DELTA, TOPIC_KEYFRAME};

/// One recorded VWP frame, with the topic it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedFrame {
    /// The MCAP topic, e.g. `"vwp/keyframe"`.
    pub topic: String,
    /// The `log_time`, which is the frame's `sim_time_ns` (§7.1).
    pub sim_time: SimTime,
    /// The frame, exactly as it was stored.
    pub frame: Frame,
}

/// One recorded serde record (build decision D11 item 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRecord {
    /// The channel, e.g. `"node.tx"`.
    pub channel: String,
    /// The record's simulated time.
    pub sim_time: SimTime,
    /// The record's JSON encoding.
    pub json: Vec<u8>,
}

/// What a seek produced — §7.3 step 8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeekResult {
    /// The preceding keyframe, with `FLAG_RESYNC | FLAG_SEEK_RESULT` set.
    pub keyframe: Frame,
    /// The deltas from just after the keyframe up to and including the target.
    pub deltas: Vec<Frame>,
    /// The keyframe's simulated time.
    pub keyframe_time: SimTime,
    /// How many chunks were read and decompressed.
    ///
    /// §7.3's "at most two" follows from a GOP fitting one chunk, which is the case at
    /// every realistic cadence and the case for every file
    /// [`crate::writer::RecordingWriter`] writes at its default target. A GOP larger than
    /// `chunk_target_bytes` is split across as many chunks as it needs, and the seek reads
    /// them: a bound the file does not honour is not a bound to read under.
    pub chunks_read: usize,
}

impl SeekResult {
    /// The simulated time the stream is positioned at after applying everything.
    pub fn position(&self) -> SimTime {
        self.deltas
            .last()
            .and_then(|d| d.sim_time().ok())
            .unwrap_or(self.keyframe_time)
    }
}

/// What `verify` found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VerifyReport {
    /// VWP frames walked.
    pub frames: u64,
    /// serde records walked.
    pub records: u64,
    /// Keyframes seen.
    pub keyframes: u64,
    /// Deltas seen.
    pub deltas: u64,
    /// Frames whose `msg_type` this build does not know.
    ///
    /// Counted, not refused: §8.4 makes a new message type additive and §8.6 has the
    /// reader "accept a higher minor, ignoring what it does not know" (conformance F6,
    /// N1). A non-zero count is the honest report of what a v1 build lost.
    pub unknown_frames: u64,
    /// `prov_id`s delivered by `Provenance` frames (conformance C5).
    pub provenance_ids: u64,
    /// References to a `prov_id` that were checked against those deliveries (C5).
    pub provenance_references: u64,
    /// The first and last snapshot times.
    pub span: Option<(SimTime, SimTime)>,
    /// The largest gap between consecutive snapshot frames, in nanoseconds.
    pub max_snapshot_gap_ns: u64,
    /// The cadence the manifest declared, or the default if it declared none.
    pub cadence: Cadence,
    /// The profile the manifest declared.
    pub profile: Profile,
    /// Chunks whose stored CRC-32 was present and matched.
    pub chunks_checksummed: u64,
    /// Chunks that declared `uncompressed_crc = 0`, so nothing in them was checked.
    ///
    /// The container defines a zero CRC as "not present", which makes the field a switch
    /// the file can throw to skip validation. Those chunks are still read — a zero CRC is
    /// legal and refusing it would reject conforming third-party files — but they are
    /// counted here, and [`VerifyReport::integrity_verified`] is false while the count is
    /// non-zero, so nothing in this crate can report a recording as verified when it
    /// checked nothing. See [`ChunkIntegrity`].
    pub chunks_without_checksum: u64,
}

impl VerifyReport {
    /// True only if every chunk walked carried a checksum and it matched.
    ///
    /// This is deliberately separate from `verify` returning `Ok`. `Ok` means the stream
    /// is self-consistent: the seq is dense, the deltas are rooted, the bodies decode. It
    /// does **not** mean the bytes are the bytes that were written, and for a chunk whose
    /// `uncompressed_crc` is zero nothing stands behind them at all. A caller reporting
    /// "verified" to a human should be asking this, not just the `Result`.
    pub const fn integrity_verified(&self) -> bool {
        self.chunks_without_checksum == 0
    }
}

/// Reads a recording.
#[derive(Debug)]
pub struct Reader<S: Source> {
    src: S,
    index: SeekIndex,
    cadence: Cadence,
    profile: Profile,
    manifest: BTreeMap<String, String>,
    require_chunk_checksums: bool,
    integrity: IntegrityTally,
}

/// How many chunks a walk checked and how many it could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct IntegrityTally {
    checksummed: u64,
    unchecked: u64,
}

impl IntegrityTally {
    /// Records one chunk's integrity, refusing it if the caller asked for a checksum.
    fn note(&mut self, chunk: usize, integrity: ChunkIntegrity, require: bool) -> Result<()> {
        match integrity {
            ChunkIntegrity::Verified => self.checksummed += 1,
            ChunkIntegrity::NotChecked => {
                if require {
                    return Err(RecordError::UncheckedChunk { chunk });
                }
                self.unchecked += 1;
            }
        }
        Ok(())
    }
}

impl Reader<MemorySource> {
    /// Opens a recording, reading the file into memory.
    ///
    /// # Errors
    /// [`RecordError::Io`] if the file cannot be read, and whatever
    /// [`SeekIndex::read`] returns for a file that is not a finished MCAP recording.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|e| RecordError::io(path, e))?;
        Self::open_bytes(bytes)
    }

    /// Opens a recording already in memory.
    ///
    /// # Errors
    /// As [`Reader::open`].
    pub fn open_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::with_source(MemorySource::new(bytes))
    }
}

impl Reader<FileSource> {
    /// Opens a recording without reading it: the footer, the summary and the message
    /// indexes only, with chunks fetched on demand (§7.4).
    ///
    /// # Errors
    /// As [`Reader::open`].
    pub fn open_paged(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_source(FileSource::open(path)?)
    }
}

impl<S: Source> Reader<S> {
    /// Opens a recording over any [`Source`].
    ///
    /// # Errors
    /// Whatever [`SeekIndex::read`] returns.
    pub fn with_source(mut src: S) -> Result<Self> {
        let index = SeekIndex::read(&mut src)?;
        let manifest = index
            .read_metadata(&mut src, METADATA_MANIFEST)
            .unwrap_or_default();
        check_version(&manifest)?;
        let cadence = cadence_from(&manifest)?;
        let profile = match manifest.get("profile").map(String::as_str) {
            Some("node") => Profile::NodeOnly,
            _ => Profile::Full,
        };
        Ok(Reader {
            src,
            index,
            cadence,
            profile,
            manifest,
            require_chunk_checksums: false,
            integrity: IntegrityTally::default(),
        })
    }

    /// Refuse a chunk that declares no checksum, instead of reading it and reporting it.
    ///
    /// Off by default, and the default is the considered one: `uncompressed_crc = 0` means
    /// "no CRC available" in the container, so refusing it outright would reject a file
    /// that conforms to the specification, and this crate would be the tool that cannot
    /// open other people's recordings. What the default does instead is *report* — see
    /// [`ChunkIntegrity`] and [`VerifyReport::integrity_verified`].
    ///
    /// Turn it on where a missing checksum is genuinely disqualifying rather than merely
    /// unusual: ingesting into an archive whose whole value is that every artefact in it is
    /// checked, or re-reading a recording this crate wrote, which always carries one, so a
    /// zero can only be damage or tampering. Every read path then fails with
    /// [`RecordError::UncheckedChunk`] naming the chunk.
    pub fn require_chunk_checksums(&mut self, require: bool) {
        self.require_chunk_checksums = require;
    }

    /// Whether this reader refuses a chunk that declares no checksum.
    pub const fn requires_chunk_checksums(&self) -> bool {
        self.require_chunk_checksums
    }

    /// The index: chunks, channels, schemas and the keyframe and delta message indexes.
    pub fn index(&self) -> &SeekIndex {
        &self.index
    }

    /// The cadence the manifest declared, or [`Cadence::DEFAULT`].
    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    /// The profile the recording was produced in.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The `v2xw.manifest` metadata record, key by key.
    pub fn manifest(&self) -> &BTreeMap<String, String> {
        &self.manifest
    }

    /// The run manifest JSON, if the recording carries one.
    pub fn manifest_json(&self) -> Option<&str> {
        self.manifest.get("json").map(String::as_str)
    }

    /// The channels the recording declares.
    pub fn channels(&self) -> impl Iterator<Item = &ChannelMeta> {
        self.index.channels.values()
    }

    /// The channels the recording declares that carry ground truth.
    ///
    /// This is what the `NODE-only` test asserts is empty: a stripped recording must
    /// contain no such channel at all, not merely no such record.
    pub fn ground_truth_channels(&self) -> impl Iterator<Item = &ChannelMeta> {
        self.index.channels.values().filter(|c| c.is_ground_truth())
    }

    /// An attachment's bytes — §7.1's `scenario.yaml` and `world.vwb`.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if the recording has no such attachment.
    pub fn attachment(&mut self, name: &str) -> Result<Vec<u8>> {
        let index = std::mem::take(&mut self.index);
        let out = index.read_attachment(&mut self.src, name);
        self.index = index;
        out
    }

    /// Walks every message in the recording, in stored order, which is the order the
    /// producer emitted them in.
    ///
    /// # Errors
    /// Whatever the chunk reader or the frame decoder returns; a corrupt chunk stops the
    /// walk with a named error rather than a panic.
    pub fn for_each_message(
        &mut self,
        mut on_frame: impl FnMut(RecordedFrame) -> Result<()>,
        mut on_record: impl FnMut(RecordedRecord) -> Result<()>,
    ) -> Result<()> {
        let index = std::mem::take(&mut self.index);
        self.integrity = IntegrityTally::default();
        let require = self.require_chunk_checksums;
        let mut tally = IntegrityTally::default();
        let result = (|| -> Result<()> {
            for chunk in 0..index.chunks.len() {
                let integrity = index.for_each_message_in_chunk(
                    &mut self.src,
                    chunk,
                    |header, data, channel| {
                        let Some(meta) = index.channels.get(&channel) else {
                            return Err(RecordError::malformed(
                                "mcap message",
                                format!("message on undeclared channel {channel}"),
                            ));
                        };
                        match meta.message_encoding.as_str() {
                            ENCODING_VWP => on_frame(RecordedFrame {
                                topic: meta.topic.clone(),
                                sim_time: header.log_time,
                                frame: Frame::from_bytes(data.to_vec())?,
                            }),
                            ENCODING_JSON => on_record(RecordedRecord {
                                channel: meta
                                    .metadata
                                    .get("channel")
                                    .cloned()
                                    .unwrap_or_else(|| meta.topic.clone()),
                                sim_time: header.log_time,
                                json: data.to_vec(),
                            }),
                            other => Err(RecordError::malformed(
                                "mcap channel",
                                format!(
                                    "topic {:?} has unknown message encoding {other:?}",
                                    meta.topic
                                ),
                            )),
                        }
                    },
                )?;
                tally.note(chunk, integrity, require)?;
            }
            Ok(())
        })();
        self.index = index;
        self.integrity = tally;
        result
    }

    /// Replays the recording as the stream the live engine produced: every VWP frame, in
    /// canonical order, exactly as stored (§7.2).
    ///
    /// # Errors
    /// As [`Reader::for_each_message`].
    pub fn replay(&mut self) -> Result<Vec<RecordedFrame>> {
        let mut out = Vec::new();
        self.for_each_message(
            |f| {
                out.push(f);
                Ok(())
            },
            |_| Ok(()),
        )?;
        Ok(out)
    }

    /// Every serde record, optionally restricted to one channel.
    ///
    /// # Errors
    /// As [`Reader::for_each_message`].
    pub fn records(&mut self, channel: Option<&str>) -> Result<Vec<RecordedRecord>> {
        let mut out = Vec::new();
        self.for_each_message(
            |_| Ok(()),
            |r| {
                if channel.is_none_or(|c| c == r.channel) {
                    out.push(r);
                }
                Ok(())
            },
        )?;
        Ok(out)
    }

    /// Walks the recording and checks its internal consistency.
    ///
    /// The checks, all of them structural rather than statistical:
    ///
    /// * every frame's magic, version and `body_len` agree with its bytes (F1, F3);
    /// * no stored frame carries a transport flag bit (§7.2 item 3, P3);
    /// * `log_time` never goes backwards;
    /// * canonical `seq` values are dense and monotonic, and a `Hello` carries the next
    ///   one without consuming it (H4);
    /// * no gap between consecutive snapshot frames exceeds the mobility step, and no
    ///   gap between keyframes exceeds the keyframe period;
    /// * every delta quotes the GOP of a keyframe that came before it, with a
    ///   `step_index` that is contiguous from 1 (§3.4);
    /// * keyframe `gop_index` increments by one;
    /// * every body decodes, including the correlation between a delta's `mflags` and its
    ///   absolute and lane blocks;
    /// * the profile flag is the same on every canonical frame and matches the keyframes'
    ///   `profile` field (§5.3);
    /// * every `prov_id` a `MetricSample` references was delivered by a `Provenance` frame
    ///   earlier in the stream (conformance C5).
    ///
    /// # What it does not check
    ///
    /// C5's other half — a `prov_id` referenced from inside an *event payload* — is not
    /// checked, because the reference's offset differs per channel and §3.6's payload
    /// tables are the province of the families that define them, not of the container. A
    /// conformance kit that decodes payloads owns that half. A `msg_type` this build does
    /// not know is counted in [`VerifyReport::unknown_frames`] and its body is not decoded,
    /// so nothing inside it is checked at all (§8.4).
    ///
    /// # `Ok` is not the same as "verified"
    ///
    /// `Ok` says the recording is *self-consistent*: it decoded, the seq is dense, the
    /// deltas are rooted, the times run forwards. It does not by itself say the bytes are
    /// the bytes that were written, because a chunk's CRC can be switched off from the
    /// wire — `uncompressed_crc = 0` is the container's "not present". Those chunks are
    /// read and counted in [`VerifyReport::chunks_without_checksum`], and
    /// [`VerifyReport::integrity_verified`] is false while that count is non-zero. Anything
    /// that reports a recording to a human as verified should consult that as well as this
    /// `Result`; anything that would rather refuse should call
    /// [`Reader::require_chunk_checksums`] first. See [`ChunkIntegrity`] for why the
    /// default reports rather than refuses.
    ///
    /// # Errors
    /// [`RecordError::Inconsistent`] naming the first failure, with the time and frame
    /// count at which it was found, and [`RecordError::UncheckedChunk`] if
    /// [`Reader::require_chunk_checksums`] is on and a chunk carries no checksum.
    pub fn verify(&mut self) -> Result<VerifyReport> {
        let cadence = self.cadence;
        let mut report = VerifyReport {
            cadence,
            profile: self.profile,
            ..Default::default()
        };
        let mut state = VerifyState {
            cadence,
            ..Default::default()
        };
        // A cell, because both closures need it: the frame walk increments it and the
        // record walk names it in an error.
        let frames = std::cell::Cell::new(0u64);
        let mut records = 0u64;
        let mut keyframes = 0u64;
        let mut deltas = 0u64;
        let mut unknown_frames = 0u64;
        let mut provenance_references = 0u64;
        let mut delivered: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        let mut first: Option<SimTime> = None;
        let mut last: SimTime = 0;
        let mut max_gap = 0u64;
        self.for_each_message(
            |f| {
                frames.set(frames.get() + 1);
                let seen = frames.get();
                let h = f.frame.header()?;
                let bad = |detail: String| RecordError::Inconsistent {
                    at: f.sim_time,
                    frames: seen,
                    detail,
                };
                if h.flags & TRANSPORT_FLAG_MASK != 0 {
                    return Err(bad(format!(
                        "stored frame carries transport flags {:#06x}; the recorder must clear them (§7.2)",
                        h.flags & TRANSPORT_FLAG_MASK
                    )));
                }
                if f.sim_time < state.last_log_time {
                    return Err(bad(format!(
                        "log_time {} is before the previous frame's {}",
                        f.sim_time, state.last_log_time
                    )));
                }
                state.last_log_time = f.sim_time;
                let Some(kind) = h.kind() else {
                    // §8.4/§8.6: ignore a message type this build does not know rather
                    // than failing the recording (conformance F6, N1). Its body is not
                    // decoded, because there is no layout for it. If its `seq` is the one
                    // the next canonical frame was due, it was a canonical frame of a
                    // later minor version and consumed that seq, so the density check
                    // continues from there; otherwise it was connection-scoped and
                    // consumed none, and nothing is assumed.
                    unknown_frames += 1;
                    if state.next_seq == Some(h.seq) {
                        state.next_seq = Some(next_dense(h.seq, "seq", f.sim_time, seen)?);
                    }
                    return Ok(());
                };
                if kind.is_canonical() {
                    match state.next_seq {
                        None => {
                            state.next_seq = Some(next_dense(h.seq, "seq", f.sim_time, seen)?);
                        }
                        Some(want) => {
                            if h.seq != want {
                                return Err(bad(format!(
                                    "canonical seq {} follows {}, which is not dense (§1.4, H4)",
                                    h.seq,
                                    // `want` is a successor this reader computed, so it is
                                    // at least one; `saturating_sub` states that rather
                                    // than relying on it, since the value it prints came
                                    // from the file.
                                    want.saturating_sub(1)
                                )));
                            }
                            state.next_seq = Some(next_dense(h.seq, "seq", f.sim_time, seen)?);
                        }
                    }
                    let node_only = h.flags & FLAG_NODE_ONLY != 0;
                    match state.node_only {
                        None => state.node_only = Some(node_only),
                        Some(prev) if prev != node_only => {
                            return Err(bad(
                                "FLAG_NODE_ONLY changes within the recording; a profile is immutable for a stream (§5.3)".to_string(),
                            ));
                        }
                        _ => {}
                    }
                } else if kind == MsgType::Hello {
                    if let Some(want) = state.next_seq {
                        if h.seq != want {
                            return Err(bad(format!(
                                "Hello carries seq {} but the next canonical frame is {want} (§2.4)",
                                h.seq
                            )));
                        }
                    }
                }
                match kind {
                    MsgType::Keyframe => {
                        keyframes += 1;
                        let kf = KeyframeBody::decode(f.frame.body()).map_err(|e| {
                            bad(format!("keyframe body does not decode: {e}"))
                        })?;
                        if kf.sim_time_ns != f.sim_time {
                            return Err(bad(format!(
                                "keyframe sim_time_ns {} disagrees with its log_time {} (§7.1)",
                                kf.sim_time_ns, f.sim_time
                            )));
                        }
                        if let Some(prev) = state.last_gop {
                            if kf.gop_index != next_dense(prev, "gop_index", f.sim_time, seen)? {
                                return Err(bad(format!(
                                    "keyframe gop_index {} does not follow {prev}",
                                    kf.gop_index
                                )));
                            }
                        }
                        if let Some(prev_kf) = state.last_keyframe_time {
                            let gap = kf.sim_time_ns.saturating_sub(prev_kf);
                            if gap > cadence.keyframe_period.as_nanos() {
                                return Err(bad(format!(
                                    "{gap} ns between keyframes exceeds the {} ns cadence",
                                    cadence.keyframe_period.as_nanos()
                                )));
                            }
                        }
                        let expected_profile = if h.flags & FLAG_NODE_ONLY != 0 {
                            crate::wire::snapshot::PROFILE_NODE
                        } else {
                            crate::wire::snapshot::PROFILE_FULL
                        };
                        if kf.profile != expected_profile {
                            return Err(bad(format!(
                                "keyframe profile {} disagrees with the frame's FLAG_NODE_ONLY (§5.3)",
                                kf.profile
                            )));
                        }
                        state.last_gop = Some(kf.gop_index);
                        state.last_keyframe_time = Some(kf.sim_time_ns);
                        state.last_step = 0;
                        state.snapshot_gap(&mut max_gap, kf.sim_time_ns)?;
                        if first.is_none() {
                            first = Some(kf.sim_time_ns);
                        }
                        last = last.max(kf.sim_time_ns);
                    }
                    MsgType::Delta => {
                        deltas += 1;
                        let d = DeltaBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("delta body does not decode: {e}")))?;
                        if d.sim_time_ns != f.sim_time {
                            return Err(bad(format!(
                                "delta sim_time_ns {} disagrees with its log_time {} (§7.1)",
                                d.sim_time_ns, f.sim_time
                            )));
                        }
                        let Some(gop) = state.last_gop else {
                            return Err(bad(
                                "delta before any keyframe: it is rooted in nothing (§3.4)".to_string(),
                            ));
                        };
                        if d.gop_index != gop {
                            return Err(bad(format!(
                                "delta quotes gop_index {} but the open GOP is {gop} (§3.4)",
                                d.gop_index
                            )));
                        }
                        if d.step_index
                            != next_dense(state.last_step, "step_index", f.sim_time, seen)?
                        {
                            return Err(bad(format!(
                                "delta step_index {} does not follow {} (§3.4.1)",
                                d.step_index, state.last_step
                            )));
                        }
                        if u64::from(d.step_index) > cadence.max_deltas_per_gop() {
                            return Err(bad(format!(
                                "delta step_index {} exceeds the {} steps a GOP can hold",
                                d.step_index,
                                cadence.max_deltas_per_gop()
                            )));
                        }
                        state.last_step = d.step_index;
                        state.snapshot_gap(&mut max_gap, d.sim_time_ns)?;
                        last = last.max(d.sim_time_ns);
                    }
                    MsgType::Telemetry => {
                        crate::wire::telemetry::TelemetryBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("telemetry body does not decode: {e}")))?;
                    }
                    MsgType::MetricSample => {
                        let m = crate::wire::metric::MetricBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("metric body does not decode: {e}")))?;
                        // C5: "every `prov_id` referenced by a `MetricSample` … has been
                        // delivered in a `Provenance` frame before it is first
                        // referenced". `0` is "none" (§3.7) and references nothing.
                        for s in &m.samples {
                            if s.prov_id == 0 {
                                continue;
                            }
                            provenance_references += 1;
                            if !delivered.contains(&s.prov_id) {
                                return Err(bad(format!(
                                    "MetricSample references prov_id {} before any Provenance frame                                      delivered it (§10.4 C5)",
                                    s.prov_id
                                )));
                            }
                        }
                    }
                    MsgType::Event => {
                        crate::wire::event::EventBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("event body does not decode: {e}")))?;
                    }
                    MsgType::Provenance => {
                        let p = crate::wire::provenance::ProvenanceBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("provenance body does not decode: {e}")))?;
                        if p.flags & crate::wire::provenance::PROV_REPLACE_ALL != 0 {
                            delivered.clear();
                        }
                        for e in &p.entries {
                            delivered.insert(e.prov_id);
                        }
                    }
                    MsgType::Hello => {
                        crate::wire::hello::HelloBody::decode(f.frame.body())
                            .map_err(|e| bad(format!("hello body does not decode: {e}")))?;
                    }
                    MsgType::WorldChunk | MsgType::Error | MsgType::Bye => {
                        return Err(bad(format!(
                            "{} must not appear in a recording (§7.1)",
                            kind.schema_name()
                        )));
                    }
                }
                Ok(())
            },
            |r| {
                records += 1;
                serde_json::from_slice::<serde_json::Value>(&r.json).map_err(|e| {
                    RecordError::Inconsistent {
                        at: r.sim_time,
                        frames: frames.get(),
                        detail: format!("record on {} is not valid JSON: {e}", r.channel),
                    }
                })?;
                Ok(())
            },
        )?;
        report.frames = frames.get();
        report.records = records;
        report.keyframes = keyframes;
        report.deltas = deltas;
        report.unknown_frames = unknown_frames;
        report.provenance_ids = delivered.len() as u64;
        report.provenance_references = provenance_references;
        report.span = first.map(|f| (f, last));
        report.max_snapshot_gap_ns = max_gap;
        report.chunks_checksummed = self.integrity.checksummed;
        report.chunks_without_checksum = self.integrity.unchecked;
        Ok(report)
    }

    /// A stable digest of everything the recording *carries*, independent of how the
    /// container laid it out.
    ///
    /// SHA-256 over, for each message in stored order, its topic, its `log_time` and its
    /// bytes. This is the number a run manifest should record
    /// (02-architecture.md §6.5's `FileDigest`), and not the digest of the file, for a
    /// reason worth stating: the `mcap` 0.25 writer emits the repeated schema and channel
    /// records of the summary section from a `HashMap`, so their order — and therefore
    /// the file's SHA-256 — varies between runs of the same program. The data section is
    /// reproducible and so is this digest; the summary is an index, and an index that
    /// reorders itself changes no content.
    ///
    /// # Errors
    /// As [`Reader::for_each_message`].
    pub fn content_digest(&mut self) -> Result<[u8; 32]> {
        use std::io::Write;
        let hasher = std::cell::RefCell::new(v2xw_core::Sha256Writer::new());
        let feed = |hasher: &std::cell::RefCell<v2xw_core::Sha256Writer>,
                    topic: &str,
                    at: SimTime,
                    bytes: &[u8]| {
            let mut h = hasher.borrow_mut();
            // Length-prefixed, so no two different message sequences can hash alike.
            let _ = h.write_all(&(topic.len() as u64).to_le_bytes());
            let _ = h.write_all(topic.as_bytes());
            let _ = h.write_all(&at.to_le_bytes());
            let _ = h.write_all(&(bytes.len() as u64).to_le_bytes());
            let _ = h.write_all(bytes);
        };
        self.for_each_message(
            |f| {
                feed(&hasher, &f.topic, f.sim_time, f.frame.as_bytes());
                Ok(())
            },
            |r| {
                feed(&hasher, &r.channel, r.sim_time, &r.json);
                Ok(())
            },
        )?;
        Ok(hasher.into_inner().finish())
    }

    /// Seeks to `t`: the preceding keyframe plus at most one keyframe period of deltas
    /// (§7.3).
    ///
    /// The keyframe comes back with `FLAG_RESYNC | FLAG_SEEK_RESULT` set (§7.3 step 8);
    /// the deltas are unchanged, so a client applies them exactly as it would live ones.
    ///
    /// # Errors
    /// [`RecordError::SeekOutOfRange`] if `t` is before the first keyframe, and whatever
    /// the chunk reader returns for a corrupt chunk.
    pub fn seek(&mut self, t: SimTime) -> Result<SeekResult> {
        let (Some(slot), Some(span)) = (
            self.index.keyframe_at_or_before(t).copied(),
            self.index.snapshot_span(),
        ) else {
            return Err(RecordError::SeekOutOfRange {
                target: t,
                min: self.index.keyframes.first().map_or(0, |k| k.sim_time),
                max: self.index.keyframes.last().map_or(0, |k| k.sim_time),
            });
        };
        if t < span.0 {
            return Err(RecordError::SeekOutOfRange {
                target: t,
                min: span.0,
                max: span.1,
            });
        }
        let index = std::mem::take(&mut self.index);
        let result = self.collect_gop(&index, &slot, t);
        self.index = index;
        result
    }

    fn collect_gop(
        &mut self,
        index: &SeekIndex,
        slot: &crate::index::MessageSlot,
        t: SimTime,
    ) -> Result<SeekResult> {
        let kf_channel = index
            .channel_id(TOPIC_KEYFRAME)
            .ok_or_else(|| RecordError::malformed("recording", "no vwp/keyframe channel"))?;
        let delta_channel = index.channel_id(TOPIC_DELTA);
        let mut keyframe: Option<Frame> = None;
        let mut deltas: Vec<Frame> = Vec::new();
        let mut chunks_read = 0usize;
        let require = self.require_chunk_checksums;
        let mut integrity_seen = IntegrityTally::default();

        // The deltas this seek wants end at `t` and at the next keyframe, whichever comes
        // first: nothing after the GOP's end belongs to it. A chunk whose earliest message
        // is after that limit cannot hold one, which both bounds the walk and terminates
        // it — and, unlike a hard count of two, stays correct for a GOP the writer had to
        // split because it was larger than the chunk target (§7.3 step 7).
        let gop_end = index
            .keyframes
            .iter()
            .map(|k| k.sim_time)
            .find(|s| *s > slot.sim_time)
            .map_or(u64::MAX, |s| s.saturating_sub(1));
        let limit = t.min(gop_end);
        let mut chunk = slot.chunk as usize;
        loop {
            chunks_read += 1;
            let integrity = index.for_each_message_in_chunk(
                &mut self.src,
                chunk,
                |header, data, channel| {
                    if channel == kf_channel && header.log_time == slot.sim_time {
                        keyframe = Some(Frame::from_bytes(data.to_vec())?);
                    } else if Some(channel) == delta_channel
                        && header.log_time > slot.sim_time
                        && header.log_time <= t
                    {
                        deltas.push(Frame::from_bytes(data.to_vec())?);
                    }
                    Ok(())
                },
            )?;
            integrity_seen.note(chunk, integrity, require)?;
            let more = index
                .chunks
                .get(chunk + 1)
                .is_some_and(|c| c.message_start_time <= limit);
            if !more {
                break;
            }
            chunk += 1;
        }

        let keyframe = keyframe.ok_or_else(|| RecordError::Inconsistent {
            at: slot.sim_time,
            frames: 0,
            detail: format!(
                "the keyframe index points at chunk {} for t = {} but no keyframe with that time is in it",
                slot.chunk, slot.sim_time
            ),
        })?;
        deltas.sort_by_key(|d| d.sim_time().unwrap_or(0));
        let flags =
            (keyframe.header()?.flags & CANONICAL_FLAG_MASK) | FLAG_RESYNC | FLAG_SEEK_RESULT;
        Ok(SeekResult {
            keyframe: keyframe.with_flags(flags),
            deltas,
            keyframe_time: slot.sim_time,
            chunks_read,
        })
    }
}

/// The value that must follow `v` in a dense sequence, refused rather than wrapped if
/// `v` is already at the top of its type.
///
/// `seq`, `gop_index` and `step_index` are all numbers a recording supplies, and `v + 1`
/// on any of them is the same defect the chunk index had: a panic in a debug build and a
/// wrap in a release build. A wrapped `seq` is the worse half — `u64::MAX + 1` becomes
/// `0`, so the *next* frame's density check compares against `0`, and the reader either
/// reports a nonsense gap or, if the file is built for it, accepts a stream that is not
/// dense. Neither is a thing `verify` may do on input it was handed.
fn next_dense<T>(v: T, what: &str, at: SimTime, frames: u64) -> Result<T>
where
    T: num_traits_lite::CheckedSucc,
{
    v.checked_succ().ok_or_else(|| RecordError::Inconsistent {
        at,
        frames,
        detail: format!(
            "{what} is at the maximum its field can hold, so the frame after it cannot exist; the            recording is not a dense sequence (§1.4)"
        ),
    })
}

/// The one operation [`next_dense`] needs, without pulling in a numeric-traits crate.
mod num_traits_lite {
    /// A counter that can say whether it has a successor.
    pub trait CheckedSucc: Copy {
        /// `self + 1`, or `None` at the type's maximum.
        fn checked_succ(self) -> Option<Self>;
    }
    impl CheckedSucc for u64 {
        fn checked_succ(self) -> Option<Self> {
            self.checked_add(1)
        }
    }
    impl CheckedSucc for u32 {
        fn checked_succ(self) -> Option<Self> {
            self.checked_add(1)
        }
    }
}

#[derive(Debug, Default)]
struct VerifyState {
    cadence: Cadence,
    last_log_time: SimTime,
    next_seq: Option<u64>,
    node_only: Option<bool>,
    last_gop: Option<u32>,
    last_step: u32,
    last_keyframe_time: Option<SimTime>,
    last_snapshot_time: Option<SimTime>,
}

impl VerifyState {
    fn snapshot_gap(&mut self, max_gap: &mut u64, now: SimTime) -> Result<()> {
        if let Some(prev) = self.last_snapshot_time {
            let gap = now.saturating_sub(prev);
            *max_gap = (*max_gap).max(gap);
            if gap > self.cadence.mobility_step.as_nanos() {
                return Err(RecordError::Inconsistent {
                    at: now,
                    frames: 0,
                    detail: format!(
                        "{gap} ns between snapshot frames exceeds the {} ns mobility step",
                        self.cadence.mobility_step.as_nanos()
                    ),
                });
            }
        }
        self.last_snapshot_time = Some(now);
        Ok(())
    }
}

/// §8.6: "The replay reader refuses a file with a higher major (error `-32050`) and
/// accepts a higher minor, ignoring what it does not know."
///
/// The version is in the `v2xw.manifest` metadata record and in each schema record; the
/// metadata record is the one a reader can consult before it decodes anything, so that is
/// what is checked here. A recording with no manifest at all — one written by a tool that
/// predates the convention — is read as v1, because every frame in it carries its own
/// major version in its header and [`Frame::from_bytes`] checks that.
fn check_version(manifest: &BTreeMap<String, String>) -> Result<()> {
    let Some(major) = manifest.get("vwp_version_major") else {
        return Ok(());
    };
    let found: u16 = major.parse().map_err(|_| {
        RecordError::malformed(
            "v2xw.manifest",
            format!("vwp_version_major = {major:?} is not a number"),
        )
    })?;
    if found > crate::wire::VERSION_MAJOR {
        return Err(RecordError::UnsupportedVersion {
            found,
            supported: crate::wire::VERSION_MAJOR,
        });
    }
    Ok(())
}

fn cadence_from(manifest: &BTreeMap<String, String>) -> Result<Cadence> {
    let parse = |key: &str| -> Option<u64> { manifest.get(key)?.parse().ok() };
    match (parse("keyframe_period_ns"), parse("mobility_step_ns")) {
        (Some(k), Some(m)) => Cadence::new(
            v2xw_core::time::Duration::from_nanos(k),
            v2xw_core::time::Duration::from_nanos(m),
        ),
        _ => Ok(Cadence::DEFAULT),
    }
}
