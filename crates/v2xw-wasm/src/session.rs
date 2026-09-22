//! The seek loop: open the index, seek to a simulated time, resolve the scene.
//!
//! This is the whole replay reader as the Studio uses it, with no dependence on the
//! browser, so `cargo test -p v2xw-wasm` exercises the same code the `wasm32` build ships
//! (09-ui §7: "the same Rust code compiled natively … or to WASM … so the Studio has no
//! replay-specific code path").
//!
//! # The seek contract
//!
//! 09-ui §7 and vwp-v1 §7.3: a seek reads the footer, the summary offsets, the chunk
//! index, then the covering chunk's message index to the nearest keyframe ≤ *t*, and
//! applies deltas up to *t*. With a keyframe every second of simulated time it touches
//! one chunk and at most one keyframe period of deltas. [`v2xw_record::Reader::seek`]
//! does that; [`ReplaySession::seek`] adds the application of the deltas to the pose
//! columns, which is what a renderer actually wants.
//!
//! # Asking for bytes
//!
//! Every entry point comes in two shapes. [`ReplaySession::open`] and
//! [`ReplaySession::seek`] return [`Step::Need`] when the bytes are not resident, naming
//! the ranges to fetch; the caller fetches them, calls [`ReplaySession::supply`] and
//! repeats. A recording that is already fully resident never yields [`Step::Need`], so
//! the `ArrayBuffer` path is the same code with the loop running once.
//!
//! # No wall clock
//!
//! Nothing in this crate reads one. The seek benchmark lives in JavaScript
//! (`bench/seek.mjs`), where the clock is the host's and the measurement is honest about
//! being a measurement.

use v2xw_core::time::SimTime;
use v2xw_record::encoder::Cadence;
use v2xw_record::error::{RecordError, Result};
use v2xw_record::wire::snapshot::{DeltaBody, KeyframeBody};
use v2xw_record::{Reader, SeekResult};

use crate::ranges::{ByteCache, SharedCache};
use crate::scene::Scene;

/// The outcome of an operation that may have run out of resident bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<T> {
    /// It finished.
    Done(T),
    /// It needs these `(offset, len)` ranges before it can finish.
    Need(Vec<(u64, u64)>),
}

impl<T> Step<T> {
    /// The value, or `None` if more bytes are wanted.
    pub fn done(self) -> Option<T> {
        match self {
            Step::Done(v) => Some(v),
            Step::Need(_) => None,
        }
    }
}

/// What a completed seek landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekReport {
    /// The keyframe the seek started from, in nanoseconds of simulated time.
    pub keyframe_time_ns: SimTime,
    /// The simulated time the scene is resolved to, which is the last delta applied or
    /// the keyframe when none was.
    pub position_ns: SimTime,
    /// How many deltas were applied.
    pub deltas: usize,
    /// How many chunks the index had to read and decompress.
    pub chunks_read: usize,
    /// How many actor slots the keyframe declared.
    pub actor_count: usize,
}

/// A replay reader positioned on a recording.
///
/// Deliberately not `Send`: the byte cache is shared with the reader's source through an
/// `Rc`, and both the browser main thread and a dedicated worker are single-threaded.
#[derive(Debug)]
pub struct ReplaySession {
    cache: SharedCache,
    reader: Option<Reader<crate::ranges::CachedSource>>,
    scene: Scene,
    last: Option<SeekReport>,
    frames: Option<SeekResult>,
}

impl ReplaySession {
    /// A session over a recording that is entirely in memory — the `ArrayBuffer` case.
    ///
    /// # Errors
    /// Whatever [`v2xw_record::Reader`] returns for a file it cannot index.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let mut s = ReplaySession::over(ByteCache::resident(bytes));
        match s.open()? {
            Step::Done(()) => Ok(s),
            Step::Need(ranges) => Err(RecordError::malformed(
                "recording",
                format!(
                    "a fully resident recording still wanted {} range(s); the first was at offset {}",
                    ranges.len(),
                    ranges.first().map_or(0, |r| r.0)
                ),
            )),
        }
    }

    /// A session over a recording of `total` bytes that will be fetched by range request.
    ///
    /// The caller drives [`ReplaySession::open`] to completion before seeking.
    pub fn ranged(total: u64) -> Self {
        ReplaySession::over(ByteCache::new(total))
    }

    /// A session over a cache the caller configured.
    pub fn over(cache: ByteCache) -> Self {
        ReplaySession {
            cache: cache.shared(),
            reader: None,
            scene: Scene::new(),
            last: None,
            frames: None,
        }
    }

    /// Adds a fetched range.
    pub fn supply(&mut self, offset: u64, bytes: Vec<u8>) {
        self.cache.with(|c| c.supply(offset, bytes));
    }

    /// How many ranges have been supplied — the round-trip cost of the scrub so far.
    pub fn requests(&self) -> u32 {
        self.cache.with(|c| c.requests())
    }

    /// How many bytes of the recording have been fetched.
    pub fn resident_bytes(&self) -> u64 {
        self.cache.with(|c| c.resident_bytes())
    }

    /// The recording's total size.
    pub fn total_bytes(&self) -> u64 {
        self.cache.with(|c| c.total())
    }

    /// The resolved pose columns.
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The last completed seek, or `None` before the first one.
    pub const fn last_seek(&self) -> Option<SeekReport> {
        self.last
    }

    /// The `Keyframe` frame the last seek returned, exactly as it was recorded except for
    /// the `FLAG_RESYNC | FLAG_SEEK_RESULT` the seek sets (§7.3 step 8).
    ///
    /// These are the bytes the live engine put on the wire. A client that already speaks
    /// VWP can feed them to the same code path it uses for a live stream, which is the
    /// byte-identity guarantee of §7.2 being useful rather than merely true.
    pub fn keyframe_frame(&self) -> Option<&[u8]> {
        self.frames.as_ref().map(|f| f.keyframe.as_bytes())
    }

    /// The `i`-th `Delta` frame of the last seek, in ascending time order.
    pub fn delta_frame(&self, i: usize) -> Option<&[u8]> {
        self.frames
            .as_ref()
            .and_then(|f| f.deltas.get(i))
            .map(|f| f.as_bytes())
    }

    /// How many delta frames the last seek returned.
    pub fn delta_frames(&self) -> usize {
        self.frames.as_ref().map_or(0, |f| f.deltas.len())
    }

    /// Reads the footer, the summary and the message indexes.
    ///
    /// Idempotent: once the index is in hand this returns [`Step::Done`] without touching
    /// the cache, so a caller may drive it in a loop without tracking state.
    ///
    /// # Errors
    /// Whatever the index reader returns for a file it refuses. Running out of resident
    /// bytes is **not** an error; it is [`Step::Need`].
    pub fn open(&mut self) -> Result<Step<()>> {
        if self.reader.is_some() {
            return Ok(Step::Done(()));
        }
        self.attempt(|cache| Reader::with_source(cache.source()))
            .map(|step| match step {
                Step::Done(reader) => {
                    self.reader = Some(reader);
                    Step::Done(())
                }
                Step::Need(r) => Step::Need(r),
            })
    }

    /// True once the index has been read.
    pub const fn is_open(&self) -> bool {
        self.reader.is_some()
    }

    /// The index the seek runs against.
    ///
    /// # Errors
    /// [`RecordError::Malformed`] if the session has not been opened.
    pub fn index(&self) -> Result<&v2xw_record::SeekIndex> {
        Ok(self.reader()?.index())
    }

    /// The cadence the manifest declared.
    ///
    /// # Errors
    /// As [`ReplaySession::index`].
    pub fn cadence(&self) -> Result<Cadence> {
        Ok(self.reader()?.cadence())
    }

    /// The first and last snapshot times in nanoseconds.
    ///
    /// # Errors
    /// As [`ReplaySession::index`].
    pub fn span(&self) -> Result<Option<(SimTime, SimTime)>> {
        Ok(self.reader()?.index().snapshot_span())
    }

    /// Seeks to `t_ns` and resolves the scene there.
    ///
    /// The scene afterwards is the keyframe at or before `t_ns` with every delta up to
    /// and including `t_ns` applied — the state the live engine held at that instant,
    /// integer for integer.
    ///
    /// # Errors
    /// [`RecordError::SeekOutOfRange`] outside the recorded span, and whatever the chunk
    /// reader or the frame decoders return for a file they refuse.
    pub fn seek(&mut self, t_ns: SimTime) -> Result<Step<SeekReport>> {
        if let Step::Need(r) = self.open()? {
            return Ok(Step::Need(r));
        }
        let result = match self.attempt_seek(t_ns)? {
            Step::Done(r) => r,
            Step::Need(r) => return Ok(Step::Need(r)),
        };
        let report = self.resolve(&result)?;
        self.last = Some(report);
        self.frames = Some(result);
        Ok(Step::Done(report))
    }

    /// Decodes the frames a seek returned and applies them to the scene.
    fn resolve(&mut self, result: &SeekResult) -> Result<SeekReport> {
        let kf = KeyframeBody::decode(result.keyframe.body())?;
        self.scene.load_keyframe(&kf);
        for frame in &result.deltas {
            let d = DeltaBody::decode(frame.body())?;
            self.scene.apply_delta(&d)?;
        }
        Ok(SeekReport {
            keyframe_time_ns: result.keyframe_time,
            position_ns: self.scene.sim_time_ns(),
            deltas: result.deltas.len(),
            chunks_read: result.chunks_read,
            actor_count: self.scene.actor_count(),
        })
    }

    fn reader(&self) -> Result<&Reader<crate::ranges::CachedSource>> {
        self.reader
            .as_ref()
            .ok_or_else(|| RecordError::malformed("recording", "the session is not open yet"))
    }

    fn attempt_seek(&mut self, t_ns: SimTime) -> Result<Step<SeekResult>> {
        // The reader is taken out for the duration so the borrow of `self` inside
        // `attempt` does not overlap with the borrow of the reader.
        let mut reader = match self.reader.take() {
            Some(r) => r,
            None => {
                return Err(RecordError::malformed(
                    "recording",
                    "the session is not open yet",
                ));
            }
        };
        let out = self.attempt(|_| reader.seek(t_ns));
        self.reader = Some(reader);
        out
    }

    /// Runs `f`, turning "the cache did not have the bytes" into [`Step::Need`] and
    /// leaving every other failure alone.
    ///
    /// The distinction is made by the cache, not by inspecting the error: a read that
    /// missed recorded what it wanted, so a non-empty want list is the signal. An error
    /// with an empty want list is a real refusal — a corrupt chunk, a bad magic, a seek
    /// past the end — and is propagated.
    fn attempt<T>(&mut self, f: impl FnOnce(&SharedCache) -> Result<T>) -> Result<Step<T>> {
        self.cache.with(|c| {
            let _ = c.take_wanted();
        });
        match f(&self.cache) {
            Ok(v) => {
                // A want recorded by a read that nevertheless succeeded (the reader
                // probes optimistically in places) is not a reason to stall.
                self.cache.with(|c| {
                    let _ = c.take_wanted();
                });
                Ok(Step::Done(v))
            }
            Err(e) => {
                let wanted = self.cache.with(|c| c.take_wanted());
                if wanted.is_empty() {
                    Err(e)
                } else {
                    Ok(Step::Need(wanted))
                }
            }
        }
    }
}
