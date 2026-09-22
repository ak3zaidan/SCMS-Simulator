//! `v2xw info` — print a recording's manifest, channels and verification report.
//!
//! # "Verified" means what the reader checked, and nothing more
//!
//! [`v2xw_record::VerifyReport::integrity_verified`] is deliberately separate from `verify`
//! returning `Ok`: `Ok` says the stream is self-consistent, while `integrity_verified` says
//! every chunk carried a CRC and it matched. A chunk may declare `uncompressed_crc = 0`,
//! which the container defines as "no checksum present" — it is read, because a zero is
//! legal, and counted. This command prints the counts and never collapses them into a
//! green word: a recording whose chunks carry no checksum is reported as
//! `integrity: NOT VERIFIED (n chunks carried no checksum)`.
//!
//! # The content digest, not the file digest
//!
//! `info` prints [`v2xw_record::Reader::content_digest`], which is what two runs of one
//! scenario must agree on. The file's own SHA-256 is not printed as a run identity,
//! because the MCAP summary section's record order is not reproducible; see
//! [`crate::run`].

use std::collections::BTreeMap;
use std::path::Path;

use v2xw_record::Reader;

use crate::error::{CliError, Result};

/// What a recording holds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InfoOutcome {
    /// The file.
    pub path: String,
    /// Its size in bytes.
    pub file_bytes: u64,
    /// The recording profile, `full` or `node`.
    pub profile: String,
    /// The keyframe period the manifest declared, nanoseconds.
    pub keyframe_period_ns: u64,
    /// The mobility step the manifest declared, nanoseconds.
    pub mobility_step_ns: u64,
    /// The run manifest, as it was stored.
    pub manifest: Option<serde_json::Value>,
    /// The manifest metadata keys that are not the manifest JSON itself.
    pub metadata: BTreeMap<String, String>,
    /// Channels, with the record count on each.
    pub channels: Vec<ChannelInfo>,
    /// Attachments, by name and size.
    pub attachments: BTreeMap<String, u64>,
    /// SHA-256 over the data section.
    pub content_digest: String,
    /// What verification found.
    pub verify: VerifyInfo,
}

/// One channel in a recording.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelInfo {
    /// The MCAP topic.
    pub topic: String,
    /// The channel name the recorder tagged, when it is a serde channel.
    pub channel: String,
    /// `vwp1` or `json`.
    pub encoding: String,
    /// The visibility tag the recorder wrote.
    pub visibility: String,
    /// How many messages landed on it.
    pub messages: u64,
}

/// What `verify` found, flattened for printing.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct VerifyInfo {
    /// VWP frames walked.
    pub frames: u64,
    /// serde records walked.
    pub records: u64,
    /// Keyframes seen.
    pub keyframes: u64,
    /// Deltas seen.
    pub deltas: u64,
    /// Frames whose message type this build does not know; counted, not refused.
    pub unknown_frames: u64,
    /// Chunks whose CRC was present and matched.
    pub chunks_checksummed: u64,
    /// Chunks that declared no CRC.
    pub chunks_without_checksum: u64,
    /// True only when every chunk carried a checksum and it matched.
    pub integrity_verified: bool,
}

/// Reads a recording and reports what is in it.
///
/// # Errors
/// [`CliError::Io`] if the file cannot be read, [`CliError::Record`] if it is not a
/// recording this build can open — including [`v2xw_record::RecordError::NoSummary`] for a
/// run that was interrupted before `finish`.
pub fn info(path: &Path) -> Result<InfoOutcome> {
    let bytes = std::fs::read(path).map_err(|e| CliError::io("cannot read recording", path, e))?;
    let file_bytes = bytes.len() as u64;
    let mut reader = Reader::open_bytes(bytes)?;

    let cadence = reader.cadence();
    let profile = match reader.profile() {
        v2xw_record::Profile::Full => "full",
        v2xw_record::Profile::NodeOnly => "node",
    };
    let mut metadata = reader.manifest().clone();
    let manifest = metadata
        .remove("json")
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
    let attachments: BTreeMap<String, u64> = reader
        .index()
        .attachments
        .iter()
        .map(|(k, (_, len))| (k.clone(), *len))
        .collect();

    // The channel list comes from the index; the per-channel counts come from one walk of
    // the data section, which is the same walk the digest and the verification use.
    // One `RefCell` rather than two closures each owning a mutable borrow: the walk hands
    // frames to one closure and records to the other, and both count into the same table.
    let counts: std::cell::RefCell<BTreeMap<String, u64>> =
        std::cell::RefCell::new(BTreeMap::new());
    reader.for_each_message(
        |f| {
            *counts.borrow_mut().entry(f.topic.clone()).or_insert(0) += 1;
            Ok(())
        },
        |r| {
            *counts
                .borrow_mut()
                .entry(format!("record/{}", r.channel))
                .or_insert(0) += 1;
            Ok(())
        },
    )?;
    let counts = counts.into_inner();

    let channels: Vec<ChannelInfo> = reader
        .index()
        .channels
        .values()
        .map(|c| ChannelInfo {
            topic: c.topic.clone(),
            channel: c
                .metadata
                .get("channel")
                .cloned()
                .unwrap_or_else(|| c.topic.clone()),
            encoding: c.message_encoding.clone(),
            visibility: c.visibility().unwrap_or("-").to_string(),
            messages: counts.get(&c.topic).copied().unwrap_or(0),
        })
        .collect();

    let content_digest = v2xw_core::hash::hex_encode(&reader.content_digest()?);
    let v = reader.verify()?;

    Ok(InfoOutcome {
        path: path.display().to_string(),
        file_bytes,
        profile: profile.to_string(),
        keyframe_period_ns: cadence.keyframe_period.as_nanos(),
        mobility_step_ns: cadence.mobility_step.as_nanos(),
        manifest,
        metadata,
        channels,
        attachments,
        content_digest,
        verify: VerifyInfo {
            frames: v.frames,
            records: v.records,
            keyframes: v.keyframes,
            deltas: v.deltas,
            unknown_frames: v.unknown_frames,
            chunks_checksummed: v.chunks_checksummed,
            chunks_without_checksum: v.chunks_without_checksum,
            integrity_verified: v.integrity_verified(),
        },
    })
}
