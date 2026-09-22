//! A [`Source`] that is fetched a range at a time, for scrubbing a recording nobody
//! downloaded.
//!
//! # Why this shape
//!
//! [`v2xw_record::index::Source`] is synchronous — `read_at(offset, len) -> bytes` — and
//! a browser cannot fetch synchronously. The two are reconciled without threads,
//! `SharedArrayBuffer` or a blocking shim by **asking and retrying**: a read of bytes the
//! cache does not hold records the range it wanted and fails, the caller drains
//! [`ByteCache::take_wanted`], issues the HTTP range requests, feeds the answers back
//! through [`ByteCache::supply`] and runs the same operation again. The reader is
//! unmodified and unaware; nothing in it blocks.
//!
//! This is the same loop a `sans_io` parser runs, and it is why the container is
//! chunk-indexed at all (vwp-v1 §7.1): a seek touches the footer, the summary and one or
//! two chunks, so a 600 s recording is scrubbed over a handful of range requests instead
//! of a download.
//!
//! # No wall clock, no randomness
//!
//! Nothing here reads a clock or an RNG. `BTreeMap` and `BTreeSet` are used rather than
//! hash containers so the ranges a given read sequence asks for are the same on every
//! run, which is what makes the request trace itself testable.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use v2xw_record::error::{RecordError, Result};
use v2xw_record::index::Source;

/// The granularity a wanted range is rounded out to before it is reported, in bytes.
///
/// A recording reader asks for small pieces — the 8-byte footer pointer, a 9-byte record
/// prefix — and asking for exactly those would double the round trips for no saving.
/// Rounding out to a 4 KiB grid turns each into one request and costs at most a page.
///
/// 4 KiB is an I/O tunable, not a model parameter: it trades bytes transferred against
/// round trips, and both sides of that trade are transport properties rather than
/// anything the simulation computes. [`ByteCache::with_block`] changes it.
///
/// It is deliberately **small**. The obvious instinct is to read far ahead, and it is
/// wrong for this container: an MCAP message index sits immediately after the chunk it
/// describes, so the per-chunk indexes are spread the whole length of the file, and a
/// read-ahead wide enough to chain one to the next is a read-ahead wide enough to
/// download the recording. Reading a few kilobytes at each of them keeps the transfer
/// proportional to the index rather than to the file.
pub const DEFAULT_BLOCK_BYTES: u64 = 4 * 1024;

/// How far past a wanted range the cache asks for, in bytes.
///
/// Zero, and measured rather than assumed. The instinct is to read far ahead, and it is
/// wrong for this container twice over: [`DEFAULT_BLOCK_BYTES`]'s grid already pulls in a
/// record's prefix and body together, so the obvious saving is mostly already had, and a
/// window wide enough to chain one chunk's message index to the next is a window wide
/// enough to download the recording.
///
/// Measured, opening a 4.5 MB 75-chunk fixture (`tests/replay.rs`): at 0 the index walk
/// cost 96 requests and 409 KB; at 4 KiB it cost 78 requests and 638 KB. So read-ahead
/// here buys 18 round trips for 229 KB — a fair trade on a high-latency link and a poor
/// one on a slow one, which is exactly the kind of choice that belongs with the caller
/// rather than in a default. [`ByteCache::set_readahead`] makes it.
pub const DEFAULT_READAHEAD_BYTES: u64 = 0;

/// Two wanted ranges closer than this are reported as one request.
///
/// Coalescing costs the bytes in the gap and saves a round trip. 16 KiB is the point at
/// which a browser's request overhead — a header block and a round trip — is worth more
/// than the gap, on any connection a browser makes.
pub const COALESCE_GAP_BYTES: u64 = 16 * 1024;

/// The bytes of a recording that are resident, and the ranges that were asked for and
/// were not.
#[derive(Debug)]
pub struct ByteCache {
    total: u64,
    block: u64,
    readahead: u64,
    /// Resident, non-overlapping, offset-keyed and merged on insert.
    blocks: BTreeMap<u64, Vec<u8>>,
    /// Ranges a read wanted and did not find, already rounded out to `block`.
    wanted: BTreeSet<(u64, u64)>,
    resident_bytes: u64,
    requests: u32,
}

impl ByteCache {
    /// An empty cache over a recording of `total` bytes.
    pub fn new(total: u64) -> Self {
        ByteCache {
            total,
            block: DEFAULT_BLOCK_BYTES,
            readahead: DEFAULT_READAHEAD_BYTES,
            blocks: BTreeMap::new(),
            wanted: BTreeSet::new(),
            resident_bytes: 0,
            requests: 0,
        }
    }

    /// An empty cache with a different rounding granularity.
    ///
    /// A `block` of zero or one means "ask for exactly what was read", which is what the
    /// request-trace tests use so the trace is the reader's own access pattern rather
    /// than a rounding artefact.
    pub fn with_block(total: u64, block: u64) -> Self {
        ByteCache {
            block: block.max(1),
            readahead: block.max(1),
            ..ByteCache::new(total)
        }
    }

    /// Sets how far past a wanted range the cache asks for.
    ///
    /// Zero means "ask for exactly what was read", which is what a test that wants the
    /// reader's own access pattern uses.
    pub fn set_readahead(&mut self, bytes: u64) {
        self.readahead = bytes;
    }

    /// A cache that already holds the whole recording.
    ///
    /// This is the `ArrayBuffer` case: the file arrived in one piece, so no read can
    /// ever miss and the retry loop runs exactly once.
    pub fn resident(bytes: Vec<u8>) -> Self {
        let total = bytes.len() as u64;
        let mut cache = ByteCache::new(total);
        cache.supply(0, bytes);
        cache
    }

    /// Wraps this cache so it can be shared with the [`Source`] handed to the reader.
    pub fn shared(self) -> SharedCache {
        SharedCache(Rc::new(RefCell::new(self)))
    }

    /// The recording's total size in bytes, which a range client learns from the
    /// `Content-Range` or `Content-Length` header of its first request.
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// How many bytes are resident.
    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// How many ranges have been supplied since the cache was created.
    ///
    /// This is the round-trip count a scrub costs, and it is the number the range-request
    /// tests assert on.
    pub const fn requests(&self) -> u32 {
        self.requests
    }

    /// True if a read has asked for bytes that are not resident.
    pub fn is_waiting(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// Takes the ranges that are wanted, coalesced and in ascending offset order.
    ///
    /// Each entry is `(offset, len)` and is already clamped to the file, so it can go
    /// straight into a `Range: bytes=offset-(offset+len-1)` header.
    pub fn take_wanted(&mut self) -> Vec<(u64, u64)> {
        let raw = std::mem::take(&mut self.wanted);
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (start, len) in raw {
            let end = start.saturating_add(len);
            match out.last_mut() {
                Some((p_start, p_len))
                    if start
                        <= p_start
                            .saturating_add(*p_len)
                            .saturating_add(COALESCE_GAP_BYTES) =>
                {
                    let p_end = p_start.saturating_add(*p_len).max(end);
                    *p_len = p_end - *p_start;
                }
                _ => out.push((start, end - start)),
            }
        }
        out
    }

    /// Adds a fetched range at `offset`, merging it with whatever is already resident.
    ///
    /// Supplying a range twice is harmless: the overlap is dropped rather than doubled,
    /// so a client that over-fetches or retries cannot corrupt the cache.
    pub fn supply(&mut self, offset: u64, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.requests = self.requests.saturating_add(1);
        let mut start = offset;
        let mut buf = bytes;
        // Drop any prefix that is already resident in the block just below this one.
        if let Some((p_off, p_bytes)) = self.blocks.range(..=start).next_back() {
            let p_end = p_off.saturating_add(p_bytes.len() as u64);
            if p_end >= start {
                let skip = (p_end - start).min(buf.len() as u64);
                buf.drain(..skip as usize);
                start = p_end;
                if buf.is_empty() {
                    return;
                }
            }
        }
        // Absorb every resident block this one now reaches.
        loop {
            let end = start + buf.len() as u64;
            let Some((&n_off, _)) = self.blocks.range(start..).next() else {
                break;
            };
            if n_off > end {
                break;
            }
            let next = self.blocks.remove(&n_off).unwrap_or_default();
            self.resident_bytes = self.resident_bytes.saturating_sub(next.len() as u64);
            let n_end = n_off.saturating_add(next.len() as u64);
            if n_end > end {
                let from = (end - n_off) as usize;
                buf.extend_from_slice(&next[from..]);
            }
        }
        self.resident_bytes = self.resident_bytes.saturating_add(buf.len() as u64);
        // Merging with the block below keeps `blocks` one entry per contiguous run,
        // which is what makes the lookup in `read_at` a single `range(..=offset)`.
        if let Some((&p_off, p_bytes)) = self.blocks.range_mut(..start).next_back() {
            if p_off.saturating_add(p_bytes.len() as u64) == start {
                p_bytes.extend_from_slice(&buf);
                return;
            }
        }
        self.blocks.insert(start, buf);
    }

    /// The resident bytes of `[offset, offset + len)`, or `None` with the range recorded
    /// as wanted.
    fn get(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let need = len as u64;
        let end = offset.saturating_add(need);
        if let Some((&b_off, bytes)) = self.blocks.range(..=offset).next_back() {
            let b_end = b_off.saturating_add(bytes.len() as u64);
            if b_end >= end {
                let from = (offset - b_off) as usize;
                return Some(bytes[from..from + len].to_vec());
            }
        }
        self.want(offset, need);
        None
    }

    /// Records `[offset, offset + len)` as wanted, rounded out to the block grid and
    /// clamped to the file.
    fn want(&mut self, offset: u64, len: u64) {
        if offset >= self.total {
            return;
        }
        let end = offset
            .saturating_add(len.max(self.readahead))
            .min(self.total);
        let start = offset - offset % self.block;
        let up = end.next_multiple_of(self.block).min(self.total);
        self.wanted.insert((start, up.saturating_sub(start).max(1)));
    }
}

/// A [`ByteCache`] shared between the caller and the [`Source`] inside the reader.
///
/// The reader takes its source by value, so the cache cannot live inside it and still be
/// fed from outside. `Rc<RefCell<…>>` rather than `Arc<Mutex<…>>` because the browser
/// main thread and a dedicated worker are both single-threaded; this type is
/// deliberately not `Send`.
#[derive(Debug, Clone)]
pub struct SharedCache(Rc<RefCell<ByteCache>>);

impl SharedCache {
    /// Runs `f` against the cache.
    pub fn with<T>(&self, f: impl FnOnce(&mut ByteCache) -> T) -> T {
        f(&mut self.0.borrow_mut())
    }

    /// A fresh [`Source`] over the same cache.
    ///
    /// One is made per attempt, because [`v2xw_record::Reader`] consumes its source and
    /// an attempt that ran out of bytes has to be repeated.
    pub fn source(&self) -> CachedSource {
        CachedSource(self.clone())
    }
}

/// The [`Source`] the reader sees: resident bytes, or a recorded want and a failure.
#[derive(Debug, Clone)]
pub struct CachedSource(SharedCache);

impl Source for CachedSource {
    fn size(&self) -> u64 {
        self.0.with(|c| c.total)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.0.with(|c| {
            c.get(offset, len).ok_or(RecordError::Truncated {
                what: "mcap range",
                at: usize::try_from(offset).unwrap_or(usize::MAX),
                need: len,
                have: 0,
            })
        })
    }
}
