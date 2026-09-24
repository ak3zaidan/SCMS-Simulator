//! The resume ring of §1.4, and the backpressure send queue of §1.5.
//!
//! Both are bounded by construction. §1.5's first sentence is the whole design constraint:
//! *the simulation loop MUST NOT block on the socket*, so every path that a slow client
//! can reach either drops under a cap or closes the connection.

use std::collections::VecDeque;

use v2xw_record::MsgType;
use v2xw_record::wire::Frame;

/// The ring holds at least two GOPs, so any retained `seq` is preceded by a retained
/// keyframe (§1.4's `DECISION`).
pub const MIN_GOPS: usize = 2;
/// The byte cap of §1.4: 8 MiB bounds memory at about ten thousand actors.
pub const MAX_RING_BYTES: usize = 8 * 1024 * 1024;
/// The frame-count cap of §1.4.
pub const MAX_RING_FRAMES: usize = 4096;

/// One retained canonical frame.
#[derive(Debug, Clone)]
pub struct RingEntry {
    /// Its canonical sequence number.
    pub seq: u64,
    /// True if it is a `Keyframe`, which is what makes a resume point usable.
    pub keyframe: bool,
    /// The frame, canonical flags only.
    pub frame: Frame,
}

/// A bounded ring of recently produced canonical frames (§1.4).
#[derive(Debug)]
pub struct ResumeRing {
    entries: VecDeque<RingEntry>,
    bytes: usize,
    /// Frames per GOP, from the cadence; the ring keeps `MIN_GOPS` of them at least.
    min_frames: usize,
}

impl ResumeRing {
    /// A ring sized for a GOP of `frames_per_gop` canonical frames.
    pub fn new(frames_per_gop: usize) -> Self {
        ResumeRing {
            entries: VecDeque::new(),
            bytes: 0,
            min_frames: frames_per_gop.saturating_mul(MIN_GOPS).max(2),
        }
    }

    /// Retains a frame, evicting from the front until every cap holds.
    ///
    /// The `MIN_GOPS` floor wins over the byte cap: evicting below two GOPs would leave a
    /// resume point with no keyframe in front of it, which is the one thing the ring
    /// exists to prevent. A single frame larger than the whole byte cap is therefore kept
    /// rather than immediately dropped.
    pub fn push(&mut self, entry: RingEntry) {
        self.bytes += entry.frame.as_bytes().len();
        self.entries.push_back(entry);
        while self.entries.len() > self.min_frames
            && (self.bytes > MAX_RING_BYTES || self.entries.len() > MAX_RING_FRAMES)
        {
            if let Some(dropped) = self.entries.pop_front() {
                self.bytes -= dropped.frame.as_bytes().len();
            }
        }
        while self.entries.len() > MAX_RING_FRAMES {
            if let Some(dropped) = self.entries.pop_front() {
                self.bytes -= dropped.frame.as_bytes().len();
            }
        }
    }

    /// True when `seq` can be resumed: it is in the ring *and* the keyframe opening its
    /// GOP is in the ring too (§1.4 rule 1).
    pub fn can_resume(&self, seq: u64) -> bool {
        let Some(at) = self.entries.iter().position(|e| e.seq == seq) else {
            return false;
        };
        self.entries.iter().take(at + 1).any(|e| e.keyframe)
    }

    /// The retained frames from `seq` onwards, oldest first.
    pub fn replay_from(&self, seq: u64) -> Vec<RingEntry> {
        self.entries
            .iter()
            .filter(|e| e.seq >= seq)
            .cloned()
            .collect()
    }

    /// The oldest retained `seq`, if anything is retained.
    pub fn first_seq(&self) -> Option<u64> {
        self.entries.front().map(|e| e.seq)
    }

    /// How many frames are retained.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many bytes are retained.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The priority class of a frame (§1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// `Hello`, `Error`, `Bye`, `WorldChunk` and every JSON-RPC text frame. Never dropped.
    P0,
    /// `Keyframe`. Never dropped, but coalesced: one queued at a time.
    P1,
    /// `Delta`. Droppable, and only all together.
    P2,
    /// `Telemetry`, `MetricSample`, `Event`. Droppable, oldest first, per class.
    P3,
}

impl Priority {
    /// The class of a message type (§1.5's table).
    pub fn of(msg_type: MsgType) -> Priority {
        match msg_type {
            MsgType::Hello | MsgType::Error | MsgType::Bye | MsgType::WorldChunk => Priority::P0,
            MsgType::Keyframe => Priority::P1,
            MsgType::Delta => Priority::P2,
            MsgType::Telemetry | MsgType::Event | MsgType::MetricSample | MsgType::Provenance => {
                Priority::P3
            }
            // `MsgType` is `#[non_exhaustive]`: a message type a later minor version adds
            // is droppable telemetry until this build knows better. It is never P0, so an
            // unknown type can never displace `Hello`, `Error` or `Bye`.
            _ => Priority::P3,
        }
    }
}

/// How many frames of each droppable class a drop removed, for `stream.drop` (§1.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DropCounts {
    /// Deltas dropped.
    pub delta: u32,
    /// Event frames dropped.
    pub event: u32,
    /// Telemetry frames dropped.
    pub telemetry: u32,
    /// Metric frames dropped.
    pub metric: u32,
}

impl DropCounts {
    /// True when nothing was dropped.
    pub fn is_empty(self) -> bool {
        self.delta == 0 && self.event == 0 && self.telemetry == 0 && self.metric == 0
    }

    /// Adds another drop's counts in.
    pub fn merge(&mut self, other: DropCounts) {
        self.delta += other.delta;
        self.event += other.event;
        self.telemetry += other.telemetry;
        self.metric += other.metric;
    }
}

/// What `enqueue` did.
#[derive(Debug, Clone, Default)]
pub struct EnqueueReport {
    /// Frames removed to make room.
    pub dropped: DropCounts,
    /// The lowest `seq` removed, if anything was.
    pub seq_first: Option<u64>,
    /// The highest `seq` removed, if anything was.
    pub seq_last: Option<u64>,
    /// True when the caller must emit a resync keyframe (§1.5).
    pub resync_pending: bool,
    /// True when even P0 could not be queued: the connection closes with 1011.
    pub fatal: bool,
}

/// One queued frame.
#[derive(Debug, Clone)]
struct Queued {
    priority: Priority,
    msg_type: MsgType,
    seq: u64,
    frame: Frame,
}

/// A bounded per-connection send queue with the priority policy of §1.5.
#[derive(Debug)]
pub struct SendQueue {
    items: VecDeque<Queued>,
    bytes: usize,
    max_frames: usize,
    max_bytes: usize,
}

impl SendQueue {
    /// The §1.5 defaults: 64 frames and 8 MiB.
    pub const DEFAULT_MAX_FRAMES: usize = 64;
    /// The §1.5 byte default.
    pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

    /// A queue with the given caps.
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        SendQueue {
            items: VecDeque::new(),
            bytes: 0,
            max_frames: max_frames.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// How many frames are queued.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many bytes are queued.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Takes the frame at the head of the queue.
    pub fn pop(&mut self) -> Option<Frame> {
        let item = self.items.pop_front()?;
        self.bytes -= item.frame.as_bytes().len();
        Some(item.frame)
    }

    /// Queues a frame, shedding load in the order §1.5 prescribes.
    ///
    /// The order matters and is not an implementation choice: deltas go first and go
    /// **all together**, because a surviving delta after a dropped one would be applied
    /// against the wrong base (§3.4). Only then are P3 frames shed oldest-first, then the
    /// queued keyframe coalesced. `fatal` means a P0 frame did not fit even after all of
    /// that, which §1.5 says closes the connection with 1011.
    pub fn enqueue(&mut self, frame: Frame) -> EnqueueReport {
        let Ok(header) = frame.header() else {
            return EnqueueReport {
                fatal: true,
                ..Default::default()
            };
        };
        let Some(msg_type) = MsgType::from_id(header.msg_type) else {
            return EnqueueReport::default();
        };
        let priority = Priority::of(msg_type);
        let size = frame.as_bytes().len();
        let mut report = EnqueueReport::default();

        // P1 coalescing happens before any capacity test: a newer keyframe replaces an
        // older queued one whether or not the queue is full (§1.5's P1 row).
        if priority == Priority::P1 {
            self.retain_dropping(|q| q.priority != Priority::P1, &mut report);
        }

        if self.would_exceed(size) {
            // 1. every queued Delta, all or nothing.
            self.retain_dropping(|q| q.priority != Priority::P2, &mut report);
            if !report.dropped.is_empty() {
                report.resync_pending = true;
            }
        }
        if self.would_exceed(size) {
            // 2. P3 oldest-first until under cap.
            while self.would_exceed(size) {
                let Some(at) = self.items.iter().position(|q| q.priority == Priority::P3) else {
                    break;
                };
                let item = self.items.remove(at).expect("index just found");
                self.bytes -= item.frame.as_bytes().len();
                note(&mut report, &item);
            }
        }
        if self.would_exceed(size) && priority != Priority::P1 {
            // 3. the queued keyframe, replaced by nothing here — it is coalesced above
            //    when a newer one arrives, and shed here only to admit a P0 frame.
            if priority == Priority::P0 {
                self.retain_dropping(|q| q.priority != Priority::P1, &mut report);
            }
        }
        if self.would_exceed(size) {
            // 4. still over cap. P0 must go out regardless; anything else is refused and
            //    the caller stops producing for this connection.
            if priority != Priority::P0 {
                return report;
            }
            if size > self.max_bytes {
                report.fatal = true;
                return report;
            }
        }

        self.bytes += size;
        self.items.push_back(Queued {
            priority,
            msg_type,
            seq: header.seq,
            frame,
        });
        report
    }

    fn would_exceed(&self, size: usize) -> bool {
        self.items.len() + 1 > self.max_frames || self.bytes + size > self.max_bytes
    }

    fn retain_dropping(&mut self, keep: impl Fn(&Queued) -> bool, report: &mut EnqueueReport) {
        let mut kept = VecDeque::with_capacity(self.items.len());
        while let Some(item) = self.items.pop_front() {
            if keep(&item) {
                kept.push_back(item);
            } else {
                self.bytes -= item.frame.as_bytes().len();
                note(report, &item);
            }
        }
        self.items = kept;
    }
}

fn note(report: &mut EnqueueReport, item: &Queued) {
    match item.msg_type {
        MsgType::Delta => report.dropped.delta += 1,
        MsgType::Event => report.dropped.event += 1,
        MsgType::Telemetry => report.dropped.telemetry += 1,
        MsgType::MetricSample => report.dropped.metric += 1,
        _ => {}
    }
    report.seq_first = Some(report.seq_first.map_or(item.seq, |s| s.min(item.seq)));
    report.seq_last = Some(report.seq_last.map_or(item.seq, |s| s.max(item.seq)));
}
