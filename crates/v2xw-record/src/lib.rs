//! `v2xw-record` — the recording container, the writer, the replay reader and the seek
//! index.
//!
//! This crate is what makes a run reviewable after the fact. Its central correctness
//! property is one sentence from `docs/protocol/vwp-v1.md` §7.2:
//!
//! > For a given run, for every canonical frame, the 24-byte header with
//! > `flags &= CANONICAL_FLAG_MASK` and the entire body are byte-identical whether the
//! > frame was produced by the live engine or by the replay reader.
//!
//! Everything here is arranged so that sentence is true by construction rather than by
//! care. The recorder stores the frame it is handed ([`writer::RecordingWriter::write_frame`]),
//! the reader hands back what it stored ([`reader::Reader::replay`]), and there is no
//! third path that rebuilds a frame from decoded state. `tests/byte_identity.rs` writes a
//! stream, replays it and compares bytes.
//!
//! # What is in here
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | Frame header, flags, message types, symbol table | [`wire`] | vwp-v1 §2 |
//! | `Hello`, `Keyframe`, `Delta`, `Telemetry`, `Event`, `MetricSample`, `Provenance` | [`wire`] submodules | vwp-v1 §3 |
//! | Pose quantisation and the delta reference rule | [`quant`] | vwp-v1 §3.2, ADR 0008 amendment |
//! | Cadence, slots, the producer state machine | [`encoder`] | vwp-v1 §3.3, §3.4 |
//! | The channel catalogue and its visibility tags | [`channels`] | 03-interfaces §14, vwp-v1 §3.6.2 |
//! | Declared float grids and the frame scan | [`grid`] | ADR 0004 §7, build decision D9 |
//! | The `NODE-only` profile | [`profile`] | vwp-v1 §5 |
//! | The MCAP recorder | [`writer`] | vwp-v1 §7.1, ADR 0008 |
//! | Self-describing schema records | [`schema`] | vwp-v1 §7.1 |
//! | The footer, summary and message-index reader | [`index`] | vwp-v1 §7.3 |
//! | Replay, `verify` and `seek` | [`reader`] | vwp-v1 §7.2, §7.3 |
//! | Parquet, Arrow IPC and JSONL exporters | [`export`] | 08-measurement §5 |
//! | A deterministic synthetic run to record | [`fixture`] | — |
//!
//! # Two encodings, and why they are not unified
//!
//! Build decision D11 item 5 fixes this and says not to re-litigate it:
//!
//! * **Snapshot channels** — `snapshot.keyframe` and `snapshot.delta` — store the VWP
//!   binary frames **verbatim**, because storing the bytes that went over the wire is
//!   what makes live and replay provably identical. It is also why ADR 0008's amendment
//!   dropped FlatBuffers for v1: a hand-specified flat layout means there is one layout,
//!   not a wire format plus a storage format.
//! * **Event, telemetry and metric channels** use the serde [`v2xw_core::Record`] path
//!   and land in Parquet or JSONL, where a self-describing columnar format is worth far
//!   more than zero copy.
//!
//! The two live on separate MCAP topic namespaces (`vwp/…` and `record/…`) with separate
//! message encodings, and a topic never carries both. A recording may hold either or
//! both: an engine that streams `Event` frames to a UI *and* exports a dataset writes
//! both, and the container keeps them apart rather than picking a winner.
//!
//! Where the wire specification's §7.1 mapping table (which maps every channel to a whole
//! VWP frame) and D11 item 5 (which puts event, telemetry and metric channels on the
//! serde path) disagree, this crate implements both rather than choosing: §7.1's mapping
//! is what a UI stream needs, D11's is what a dataset needs, and the byte-identity
//! guarantee attaches to whatever was actually stored.
//!
//! # Reading a file this crate did not write
//!
//! A recording is routinely read half-written, and sometimes it is read hostile. Two
//! promises hold on every read path, and both are stated where they are kept — in
//! [`error`] and in [`index`] — because both were once false:
//!
//! * **Malformed input is never a panic, an abort or an acceptance.** Every offset,
//!   length, count and capacity derived from a file is computed with checked arithmetic
//!   and bounded by the buffer it claims to describe. Unchecked arithmetic on a wire value
//!   fails both ways at once: it panics in a debug build, and in a release build it wraps,
//!   so a sum that should have been rejected as past the end of the file becomes a small
//!   number that walks through the bounds check meant to stop it. `tests/overflow.rs` runs
//!   in both profiles for that reason.
//! * **Nothing is reported as verified that was not checked.** A chunk's CRC can be
//!   switched off from the wire — `uncompressed_crc = 0` is the container's "not present" —
//!   so such a chunk is read, because a zero is legal, and counted in
//!   [`VerifyReport::chunks_without_checksum`], so [`VerifyReport::integrity_verified`]
//!   goes false. [`Reader::require_chunk_checksums`] turns it into a refusal for a caller
//!   that wants one. See [`ChunkIntegrity`].
//!
//! # The rules this crate is written under
//!
//! * No `std` transcendental: every one goes through [`v2xw_core::math`] (ADR 0003,
//!   ADR 0004 §4). In practice this crate barely needs one — quantisation is `*`, `/` and
//!   `round`, all IEEE-754 exact operations — and the only transcendental constant in it
//!   is 2π, for binary radians.
//! * No `std` `HashMap` or `HashSet` reaches an output ordering or a hash: every table
//!   that decides what a file contains is a `BTreeMap`, a `BTreeSet` or an explicitly
//!   sorted `Vec`.
//! * No wall-clock read. Every time in a recording comes from [`v2xw_core::SimTime`]. The
//!   one exception in the crate is `benches/seek.rs`, which measures latency and
//!   therefore must read a clock; it is a benchmark and never linked into the engine.
//! * Every float reaching a recorded or exported artefact is quantised at the writer
//!   ([`grid`], [`export`]), and a scan reads the artefacts back and checks it.
//! * No `unsafe`, and every public item documented.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod channels;
pub mod encoder;
pub mod error;
pub mod export;
pub mod fixture;
pub mod grid;
pub mod index;
pub mod profile;
pub mod quant;
pub mod reader;
pub mod schema;
pub mod wire;
pub mod writer;

pub use channels::{CHANNELS, ChannelSpec};
pub use encoder::{
    ActorPose, Cadence, SignalState, SlotAllocator, Snapshot, SnapshotEncoder, SnapshotFrame,
};
pub use error::{RecordError, Result};
pub use export::{ExportFormat, ExportProfile, ExportedFile, Exporter, TableSchema};
pub use index::{
    ChunkIntegrity, ChunkSpan, FileSource, MemorySource, MessageSlot, SeekIndex, Source,
};
pub use profile::{NodeProfileStripper, Profile};
pub use quant::{DeltaStep, PoseRef};
pub use reader::{Reader, RecordedFrame, RecordedRecord, SeekResult, VerifyReport};
pub use wire::{Frame, FrameHeader, MsgType, StrTable};
pub use writer::{RecordingOptions, RecordingSummary, RecordingWriter};
