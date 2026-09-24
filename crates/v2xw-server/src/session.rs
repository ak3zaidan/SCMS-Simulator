//! The connection state machine of §1: the handshake, resume, subscriptions and framing.
//!
//! One [`Session`] per client stream. It owns everything that is connection-scoped and
//! nothing that is run-scoped. A session outlives the socket it started on: when the socket
//! drops, [`crate::resume`] keeps the session — and keeps encoding the run into its ring —
//! so a reconnect that names its token resumes it (§1.4). "Connection" below means that
//! stream, not one TCP socket.
//!
//! | Connection-scoped | Why |
//! |---|---|
//! | profile (§5.3) | immutable per connection; blanking happens at the producer |
//! | symbol table (§2.5) | append-only per connection, reset by a non-resumed `Hello` |
//! | `seq`, GOP and step index | the stream this connection sees, not the run's |
//! | resume ring (§1.4) | what *this* client can be replayed |
//! | send queue (§1.5) | one slow client must not slow another |
//! | telemetry and event subscriptions (§6.7, §6.12) | which rows are in a frame at all |
//! | camera and overlays (§6.7) | the client's own view state |
//!
//! # `seq` is per connection, and the specification is read that way on purpose
//!
//! §1.4 says `seq` is "assigned by the producer … in canonical emission order" and is
//! "deterministic: the same run replayed produces the same `seq` for the same frame".
//! Taken alone that suggests one counter per run. It cannot be: §6.7 makes `Telemetry`
//! carry only subscribed nodes and §6.12 makes no event channel subscribed by default, so
//! two clients of one run see different canonical frames and a shared counter would leave
//! gaps in both — which §1.4 itself tells the client to treat as a drop. The reading that
//! holds every rule at once is that the *stream* is the producer, not the run: each
//! connection's `seq` is dense from 0 (conformance H4), and determinism is the statement
//! that two clients making the same subscriptions in the same order see the same numbers.
//! Replay of a recording is unaffected, because a recording has exactly one stream in it.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::time::SimTime;
use v2xw_record::encoder::SnapshotEncoder;
use v2xw_record::profile::Profile;
use v2xw_record::wire::event::{EventBody, EventEntry};
use v2xw_record::wire::hello::{
    HELLO_LIVE, HELLO_NODE_ONLY, HELLO_PAUSED, HELLO_REPLAY, HELLO_RESUMED, HELLO_SEEKABLE,
    HELLO_WRITABLE, NODE_IS_ATTACKER, NodeRow,
};
use v2xw_record::wire::metric::MetricBody;
use v2xw_record::wire::provenance::ProvenanceBody;
use v2xw_record::wire::telemetry::TelemetryBody;
use v2xw_record::wire::{FLAG_END_OF_RUN, FLAG_RESYNC, FLAG_SEEK_RESULT, Frame, MsgType, StrTable};

use crate::engine::{RunDescriptor, RunState, StepOutput};
use crate::error::{Result, ServerError};
use crate::ring::{DropCounts, EnqueueReport, ResumeRing, SendQueue};

/// The compression the client asked for (`?compress=`).
///
/// **Not yet applied on the wire.** §2.6 says the server *SHOULD* compress a body of
/// 4096 bytes or more; this build sends every body uncompressed, with `FLAG_COMPRESSED`
/// clear, which every conforming client accepts because the flag is what tells it which
/// to expect. The consequence is bandwidth, not correctness: a 10,000-actor keyframe goes
/// out at about 280 KB instead of 60-90 KB. The parameter is parsed and rejected when
/// malformed so that a client that cannot decompress zstd is already honoured today and
/// keeps working when compression lands; turning it on is a `zstd` dependency and one
/// branch in [`Session::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// zstd, the default.
    Zstd,
    /// No compression; a client that cannot decompress zstd must ask for this.
    None,
}

/// The parsed `/vwp/v1` query string (§1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectParams {
    /// The run to attach to; `None` means the server's current run.
    pub run: Option<String>,
    /// The canonical `seq` to resume from (§1.4).
    pub resume: Option<u64>,
    /// The session token a previous `Hello` issued (§1.4, `?session=`): which retained
    /// session a reconnect is resuming. Without it `resume` names a `seq` of nothing.
    pub session: Option<String>,
    /// `full` or `node` (§5). Immutable for the connection.
    pub profile: Profile,
    /// Whether the client can decompress zstd.
    pub compress: Compression,
    /// The protocol major version the client speaks.
    pub version: u16,
}

impl Default for ConnectParams {
    fn default() -> Self {
        ConnectParams {
            run: None,
            resume: None,
            session: None,
            profile: Profile::Full,
            compress: Compression::Zstd,
            version: 1,
        }
    }
}

impl ConnectParams {
    /// Parses a raw query string.
    ///
    /// Unknown parameters are ignored, per §8.4's additive rule. A malformed known one is
    /// an error rather than a default, because silently serving `full` to a client that
    /// asked for `node` would leak ground truth.
    ///
    /// # Errors
    /// [`ServerError::InvalidParams`] for a `profile`, `compress`, `resume` or `v` value
    /// that is not one of the listed ones.
    pub fn parse(query: &str) -> Result<Self> {
        let mut out = ConnectParams::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "run" => {
                    if !value.is_empty() && value != "latest" {
                        out.run = Some(value.to_string());
                    }
                }
                "resume" => {
                    out.resume = Some(value.parse::<u64>().map_err(|_| {
                        ServerError::param(
                            "/resume",
                            "not a u64",
                            "pass the seq one past the last frame applied",
                        )
                    })?);
                }
                "session" => {
                    // Opaque: whatever a `Hello` issued. An empty value is no token.
                    if !value.is_empty() {
                        out.session = Some(value.to_string());
                    }
                }
                "profile" => {
                    out.profile = match value {
                        "full" => Profile::Full,
                        "node" => Profile::NodeOnly,
                        _ => {
                            return Err(ServerError::param(
                                "/profile",
                                "must be `full` or `node`",
                                "omit it for `full`",
                            ));
                        }
                    };
                }
                "compress" => {
                    out.compress = match value {
                        "zstd" => Compression::Zstd,
                        "none" => Compression::None,
                        _ => {
                            return Err(ServerError::param(
                                "/compress",
                                "must be `zstd` or `none`",
                                "pass compress=none if you cannot decompress zstd",
                            ));
                        }
                    };
                }
                "v" => {
                    out.version = value
                        .parse::<u16>()
                        .map_err(|_| ServerError::param("/v", "not a u16", "pass v=1"))?;
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// The camera state §6.7's `view.camera` echoes back.
#[derive(Debug, Clone)]
pub struct CameraState {
    /// `map`, `chase`, `dashboard`, `free`, `rsu` or `jump`.
    pub mode: String,
    /// Eye position, ENU metres.
    pub position: [f64; 3],
    /// Look-at point, ENU metres.
    pub target: [f64; 3],
    /// Vertical field of view, degrees.
    pub fov_deg: f64,
    /// `perspective` or `orthographic`.
    pub projection: String,
}

impl Default for CameraState {
    fn default() -> Self {
        CameraState {
            mode: "map".to_string(),
            position: [0.0, 0.0, 800.0],
            target: [0.0, 0.0, 0.0],
            fov_deg: 50.0,
            projection: "perspective".to_string(),
        }
    }
}

/// The overlay catalogue of §6.7, with the visibility tag each name implies.
///
/// A name ending in `_gt` is ground truth, which §6.7's closing sentence states and §5.3
/// turns into a `-32040` in the `node` profile.
pub const OVERLAYS: [&str; 18] = [
    "tx_pulses",
    "links",
    "cbr_heatmap",
    "coverage",
    "attackers_gt",
    "revoked",
    "reported",
    "detections",
    "backend_flows",
    "focus_region",
    "lane_markings",
    "buildings",
    "labels",
    "trajectories_gt",
    "belief_vs_truth_gt",
    "signal_state",
    "rsu_range",
    "density",
];

/// True for an overlay whose content is ground truth (§6.7).
pub fn overlay_is_gt(name: &str) -> bool {
    name.ends_with("_gt")
}

/// What the session wants the transport to do after it produced frames.
#[derive(Debug, Clone, Default)]
pub struct StepEffects {
    /// Frames to write, in order.
    pub frames: Vec<Frame>,
    /// A `stream.drop` notification to send, as `(seq_first, seq_last, counts, resync_seq)`.
    pub drop_notice: Option<(u64, u64, DropCounts, u64)>,
    /// True when the connection must be closed with 1011 (§1.5's P0 rule).
    pub fatal: bool,
}

/// A connection's `node.feed` subscription (vwp-v1 §6.7 `view.follow {feed}`).
///
/// The feed is the followed node's messages and queues, pushed as a JSON-RPC notification
/// while the node is followed and only then. `after` is the stream instant the last push
/// covered, so each push carries what is new; the transport paces pushes at `hz` on its own
/// clock, which is a transport concern and moves no simulated quantity.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedSub {
    /// The node whose feed this is: always the followed node.
    pub node: u32,
    /// What one push carries.
    pub limits: crate::feed::FeedLimits,
    /// The most pushes per wall-clock second.
    pub hz: f64,
    /// The stream instant the last push covered.
    pub after: Option<SimTime>,
}

/// One client connection.
#[derive(Debug)]
pub struct Session {
    params: ConnectParams,
    profile: Profile,
    /// The canonical sequence counter for this stream (§1.4).
    ///
    /// The snapshot encoder keeps its own, but it only ever sees snapshot frames, and
    /// `seq` runs across *every* canonical frame. Frames the encoder produces are
    /// renumbered onto this counter, which is a header rewrite and leaves the body — the
    /// part §7.2's byte-identity guarantee is about — untouched.
    seq: u64,
    /// The `NODE-only` stripper, present only on a node-profile replay connection.
    stripper: Option<v2xw_record::NodeProfileStripper>,
    encoder: SnapshotEncoder,
    ring: ResumeRing,
    queue: SendQueue,
    strings: StrTable,
    /// Nodes subscribed to `Telemetry` (§6.7).
    telemetry_nodes: BTreeSet<u32>,
    /// Event channel ids subscribed (§6.12). Empty by default, per the §6.12 decision.
    event_channels: BTreeSet<u16>,
    /// The event-filter node allow-list, empty meaning "any node".
    event_nodes: BTreeSet<u32>,
    /// `max_events_per_step` (§6.12).
    max_events_per_step: usize,
    /// `filter.sample_1_in` (§6.12).
    sample_1_in: usize,
    /// Enabled overlays, all off until the client asks (§6.7).
    overlays: BTreeMap<String, bool>,
    /// Overlay opacities.
    opacity: BTreeMap<String, f64>,
    /// The camera the client last set.
    camera: CameraState,
    /// The node `view.follow` is following.
    following: Option<u32>,
    /// The followed node's `node.feed` subscription, while there is one.
    feed: Option<FeedSub>,
    /// True once §1.5 says a resync keyframe is owed.
    resync_pending: bool,
    /// Drops not yet reported in a `stream.drop` notification.
    pending_drops: DropCounts,
    /// The `seq` range those drops covered.
    drop_span: Option<(u64, u64)>,
    /// The run's provenance, until this connection's first keyframe has carried it (§3.8).
    provenance: Option<ProvenanceBody>,
    /// The quantisation origin, kept so a seek can rebuild the encoder.
    origin_m: [f64; 3],
    /// The cadence, kept for the same reason.
    cadence: v2xw_record::encoder::Cadence,
    /// How many strings the run's own table held, so a §3.8 extension's ids can be
    /// rebased onto this connection's table. See [`Session::provenance_frame`].
    hello_base: usize,
    /// True once `Hello` has been sent.
    greeted: bool,
    /// Whether the handshake resumed an existing stream (§1.4 rule 1).
    resumed: bool,
    /// The [`crate::Run`] generation this session's `Hello` described. A step of any
    /// other generation is never encoded against this session's tables.
    generation: u64,
    /// The opaque token every `Hello` on this connection echoes (§3.1.1).
    session_token: String,
    /// Where the symbol table grew inside the stream: `(seq, length before)` for every
    /// canonical frame that appended strings (a §3.8 extension). A resumed `Hello` must
    /// carry the table *as the client held it at the resume point*, not as it is now, or
    /// the replayed frames' extensions would append their strings a second time and every
    /// later id would resolve one string off. Pruned to what the ring still holds.
    string_marks: std::collections::VecDeque<(u64, usize)>,
}

impl Session {
    /// A session for a connection to `descriptor`.
    pub fn new(params: ConnectParams, descriptor: &RunDescriptor) -> Self {
        let profile = params.profile;
        let frames_per_gop =
            usize::try_from(descriptor.cadence.max_deltas_per_gop() + 1).unwrap_or(16);
        Session {
            profile,
            seq: 0,
            stripper: if profile.is_node_only() && !descriptor.live {
                Some(v2xw_record::NodeProfileStripper::new())
            } else {
                None
            },
            encoder: SnapshotEncoder::new(descriptor.origin_m, descriptor.cadence, profile, 0),
            ring: ResumeRing::new(frames_per_gop),
            queue: SendQueue::new(SendQueue::DEFAULT_MAX_FRAMES, SendQueue::DEFAULT_MAX_BYTES),
            strings: descriptor.hello.strings.clone(),
            telemetry_nodes: BTreeSet::new(),
            event_channels: BTreeSet::new(),
            event_nodes: BTreeSet::new(),
            max_events_per_step: 5_000,
            sample_1_in: 1,
            overlays: OVERLAYS.iter().map(|n| ((*n).to_string(), false)).collect(),
            opacity: BTreeMap::new(),
            camera: CameraState::default(),
            following: None,
            feed: None,
            resync_pending: false,
            pending_drops: DropCounts::default(),
            drop_span: None,
            provenance: descriptor.provenance.clone(),
            origin_m: descriptor.origin_m,
            cadence: descriptor.cadence,
            hello_base: descriptor.hello.strings.strings.len(),
            greeted: false,
            resumed: false,
            generation: 0,
            session_token: String::new(),
            string_marks: std::collections::VecDeque::new(),
            params,
        }
    }

    /// Binds this session to the run generation its first `Hello` describes and to the
    /// token that `Hello` echoes.
    pub fn bind(&mut self, generation: u64, session_token: &str) {
        self.generation = generation;
        self.session_token = session_token.to_string();
    }

    /// The run generation this session's `Hello` described.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The token this session's `Hello` issued.
    pub fn session_token(&self) -> &str {
        &self.session_token
    }

    /// Hands this retained session to a new connection that asked to resume at `resume`.
    ///
    /// The profile, subscriptions, overlays, camera, follow, encoder, ring and `seq` are the
    /// session's and stay; what belonged to the *old socket* does not. Its send queue held
    /// frames that socket never wrote — every one of them is in the ring, which is what the
    /// resumed `Hello` replays from — so the queue is emptied rather than sent twice, and
    /// drop accounting starts again.
    ///
    /// A profile is immutable for a session (§5.3): a reconnect asking for a different one
    /// is not a resume of this session and the caller must not hand it over; see
    /// [`Session::accepts`].
    pub fn reattach(&mut self, resume: Option<u64>) {
        self.params.resume = resume;
        self.queue = SendQueue::new(SendQueue::DEFAULT_MAX_FRAMES, SendQueue::DEFAULT_MAX_BYTES);
        self.pending_drops = DropCounts::default();
        self.drop_span = None;
        self.greeted = false;
        self.resumed = false;
    }

    /// Whether a reconnect with `params` may take this session over: the same profile.
    pub fn accepts(&self, params: &ConnectParams) -> bool {
        params.profile == self.profile
    }

    /// True when `seq` can be resumed on this session (§1.4 rule 1).
    ///
    /// Either the ring holds `seq` behind a retained keyframe, or `seq` is exactly the next
    /// frame this session will produce — a client that missed nothing, which the ring
    /// cannot vouch for because the frame does not exist yet, and which is the commonest
    /// reconnect of all: a drop while the run was paused.
    pub fn can_resume(&self, seq: u64) -> bool {
        if seq == self.seq {
            // Resuming at the head needs a keyframe the client already applied, which is
            // true of any seq past the first frame of the stream, and vacuous at 0.
            return true;
        }
        self.ring.can_resume(seq)
    }

    /// The length the symbol table had when frame `seq` was about to be sent.
    fn table_len_at(&self, seq: u64) -> usize {
        self.string_marks
            .iter()
            .find(|(at, _)| *at >= seq)
            .map_or(self.strings.strings.len(), |(_, len)| *len)
    }

    /// The session for this same connection on the run `descriptor` describes.
    ///
    /// What belongs to the *connection* survives — its profile, its event-channel
    /// subscription and its caps, its overlays and camera. What belongs to the *run* does
    /// not: `seq` starts again at 0 (§1.4: "starting at 0 for the first canonical frame of
    /// the run"), the symbol table, the snapshot encoder and the resume ring are the new
    /// run's, and node-scoped subscriptions are dropped because node ids are not stable
    /// across runs — a follow of node 12 in one run is a follow of an unrelated vehicle,
    /// or of nothing, in the next.
    fn for_new_run(&self, descriptor: &RunDescriptor) -> Session {
        let mut params = self.params.clone();
        params.resume = None;
        let mut next = Session::new(params, descriptor);
        next.event_channels = self.event_channels.clone();
        next.max_events_per_step = self.max_events_per_step;
        next.sample_1_in = self.sample_1_in;
        next.overlays = self.overlays.clone();
        next.opacity = self.opacity.clone();
        next.camera = self.camera.clone();
        next.session_token = self.session_token.clone();
        next
    }

    /// Moves this connection onto the run's current generation and returns what it must
    /// send, in order: a fresh, non-resumed `Hello` (§6.6's `run.start`: "the server sends
    /// a fresh `Hello` on this connection before the first `Keyframe` of the new run") and,
    /// when the new run is not advancing, the state at its stream position.
    ///
    /// # Errors
    /// As [`Session::hello_frame_with_nodes`].
    pub fn regreet(&mut self, run: &crate::Run) -> Result<Vec<Frame>> {
        let generation = run.generation();
        let descriptor = run.descriptor();
        *self = self.for_new_run(&descriptor);
        self.generation = generation;
        self.greet(run, &descriptor)
    }

    /// The `Hello` for this connection and, when the run is not advancing, the state at its
    /// stream position (see [`crate::engine::Engine::current`]).
    ///
    /// # Errors
    /// As [`Session::hello_frame_with_nodes`].
    pub fn greet(&mut self, run: &crate::Run, descriptor: &RunDescriptor) -> Result<Vec<Frame>> {
        let state = run.state();
        let live = run.live_node_table();
        let token = self.session_token.clone();
        let hello = self.hello_frame_with_nodes(
            descriptor,
            state,
            run.sim_time(),
            &token,
            live.as_ref()
                .map(|(nodes, strings)| (&nodes[..], &strings[..])),
        )?;
        let mut frames = vec![hello];
        if !self.resumed && state != RunState::Running {
            if let Some(current) = run.current() {
                if current.generation == self.generation {
                    frames.extend(self.encode_current(&current)?);
                }
            }
        }
        Ok(frames)
    }

    /// Encodes the state at the stream position as a `FLAG_RESYNC` keyframe, for a
    /// connection that attached to a run that is not moving.
    ///
    /// The step's events and metric samples are left out: they happened before this
    /// connection existed, and replaying them would draw a burst of transmissions that
    /// is not happening. `FLAG_END_OF_RUN` is left off for the same reason — this is a
    /// view of the last instant, not the end of the stream arriving again.
    ///
    /// # Errors
    /// As [`Session::encode_step`].
    pub fn encode_current(&mut self, out: &StepOutput) -> Result<Vec<Frame>> {
        self.encoder = SnapshotEncoder::new(self.origin_m, self.cadence, self.profile, self.seq);
        self.encoder.request_keyframe();
        self.resync_pending = true;
        let mut view = out.clone();
        view.events.clear();
        view.metrics.clear();
        view.end_of_run = false;
        Ok(self.encode_step(&view)?.frames)
    }

    /// The connection's parameters.
    pub fn params(&self) -> &ConnectParams {
        &self.params
    }

    /// The connection's profile, which no control method can change (§5.3, V4).
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The `seq` the next canonical frame will carry.
    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Takes the next canonical sequence number.
    fn take_seq(&mut self) -> u64 {
        let seq = self.seq;
        self.seq += 1;
        seq
    }

    /// The camera state.
    pub fn camera(&self) -> &CameraState {
        &self.camera
    }

    /// The node being followed, if any.
    pub fn following(&self) -> Option<u32> {
        self.following
    }

    /// The `node.feed` subscription, if the followed node has one.
    pub fn feed(&self) -> Option<&FeedSub> {
        self.feed.as_ref()
    }

    /// Subscribes the followed node to `node.feed`, or drops the subscription.
    pub fn set_feed(&mut self, feed: Option<FeedSub>) {
        self.feed = feed.filter(|f| Some(f.node) == self.following);
    }

    /// Records that a push covered the stream up to `t`.
    pub fn feed_covered(&mut self, t: SimTime) {
        if let Some(f) = &mut self.feed {
            f.after = Some(t);
        }
    }

    /// The nodes subscribed to telemetry, in id order.
    pub fn telemetry_nodes(&self) -> Vec<u32> {
        self.telemetry_nodes.iter().copied().collect()
    }

    /// The subscribed event channel ids, ascending.
    pub fn event_channels(&self) -> Vec<u16> {
        self.event_channels.iter().copied().collect()
    }

    /// The overlay states, in name order.
    pub fn overlays(&self) -> &BTreeMap<String, bool> {
        &self.overlays
    }

    /// How many frames and bytes are waiting for the socket (conformance H7).
    pub fn queued(&self) -> (usize, usize) {
        (self.queue.len(), self.queue.bytes())
    }

    /// The resume ring, for tests and for `run.status`.
    pub fn ring(&self) -> &ResumeRing {
        &self.ring
    }

    /// Builds the `Hello` frame for this connection (§3.1, §1.3, §1.4).
    ///
    /// Sets the connection-scoped flags, decides resumability against the ring, and —
    /// when the handshake is *not* a resume — resets the symbol table to the run's own
    /// table (§2.5's "a non-resumed `Hello` resets the table", conformance C7).
    pub fn hello_frame(
        &mut self,
        descriptor: &RunDescriptor,
        state: RunState,
        sim_time: SimTime,
        session_token: &str,
    ) -> Result<Frame> {
        self.hello_frame_with_nodes(descriptor, state, sim_time, session_token, None)
    }

    /// As [`Session::hello_frame`], with the node table the run has *now*.
    ///
    /// §3.1.3 makes the node table "the set known at connect time", and a live run's set
    /// grows — the Phase 1 Manhattan scenario has no node until its demand model produces
    /// a vehicle. [`RunDescriptor`] is fixed when the run is wrapped, so the live table
    /// arrives here separately and its two strings are interned into the table *this*
    /// `Hello` establishes, which is what makes the ids resolvable on this connection.
    ///
    /// # Errors
    /// As [`Session::hello_frame`].
    pub fn hello_frame_with_nodes(
        &mut self,
        descriptor: &RunDescriptor,
        state: RunState,
        sim_time: SimTime,
        session_token: &str,
        nodes: Option<(&[crate::engine::NodeFacts], &[String])>,
    ) -> Result<Frame> {
        let resume_target = self.params.resume.filter(|seq| self.can_resume(*seq));
        self.resumed = resume_target.is_some();
        if !self.resumed {
            self.strings = descriptor.hello.strings.clone();
            self.string_marks.clear();
            // The client throws its table away on a non-resumed `Hello` (§2.5), including the
            // strings the run's provenance appended, so the provenance goes out again after
            // the resync keyframe. A retained session that falls back to §1.4 rule 2 has
            // already sent it once; without this its metrics would name nothing.
            self.provenance = descriptor.provenance.clone();
        }

        let mut body = descriptor.hello.clone();
        let mut flags = if descriptor.live {
            HELLO_LIVE
        } else {
            HELLO_REPLAY
        } | HELLO_WRITABLE;
        if descriptor.seekable {
            flags |= HELLO_SEEKABLE;
        }
        if self.profile.is_node_only() {
            flags |= HELLO_NODE_ONLY;
        }
        if state != RunState::Running {
            flags |= HELLO_PAUSED;
        }
        if self.resumed {
            flags |= HELLO_RESUMED;
        }
        body.hello_flags = flags;
        if let Some(seq) = resume_target {
            body.resume_seq = seq;
        } else {
            body.resume_seq = self.seq;
        }
        body.sim_time_ns = sim_time;
        if let Some(seq) = resume_target {
            // §1.4 rule 1 / §2.5: the client keeps its table, and the replay that follows
            // extends it exactly as the original frames did. So this `Hello` carries the
            // table as it stood at `seq` and appends nothing: an id interned here would take
            // the slot a replayed §3.8 extension is about to append into. A node whose label
            // is not in that table yet is labelled with id 0, the empty string, until the
            // next non-resumed `Hello`; the label is cosmetic, the ids are not.
            let len = self.table_len_at(seq);
            body.strings = StrTable {
                strings: self.strings.strings[..len.min(self.strings.strings.len())].to_vec(),
            };
            if let Some((nodes, _)) = nodes {
                let find = |t: &StrTable, s: &str| {
                    t.strings
                        .iter()
                        .position(|x| x == s)
                        .and_then(|i| u32::try_from(i).ok())
                        .unwrap_or(0)
                };
                body.nodes = nodes
                    .iter()
                    .map(|facts| NodeRow {
                        node_id: facts.node_id,
                        actor_id: facts.actor_id,
                        pos_m: facts.pos_m,
                        str_label: find(&body.strings, &facts.label),
                        str_profile_id: find(&body.strings, &facts.profile_id),
                        flags: facts.flags,
                        kind: facts.kind,
                        class_idx: facts.class_idx,
                    })
                    .collect();
            }
            body.str_session_token = body
                .strings
                .strings
                .iter()
                .position(|x| x == session_token)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            return self.finish_hello(body);
        }
        body.strings = self.strings.clone();
        if let Some((nodes, appended)) = nodes {
            // §2.5 is append-only and an id must mean one string for the whole run, so the
            // run's own appended strings go on in the order it appended them — *before*
            // any label is resolved, and including labels of nodes that have since left.
            // Resolving a label with `intern` after this is therefore a lookup, not an
            // append, and two connections agree about every id.
            for string in appended {
                body.strings.intern(string);
            }
            body.nodes = nodes
                .iter()
                .map(|facts| NodeRow {
                    node_id: facts.node_id,
                    actor_id: facts.actor_id,
                    pos_m: facts.pos_m,
                    str_label: body.strings.intern(&facts.label),
                    str_profile_id: body.strings.intern(&facts.profile_id),
                    flags: facts.flags,
                    kind: facts.kind,
                    class_idx: facts.class_idx,
                })
                .collect();
        }
        body.str_session_token = body.strings.intern(session_token);
        self.strings = body.strings.clone();
        self.finish_hello(body)
    }

    /// The part of building a `Hello` that is the same for a fresh and a resumed one: the
    /// profile's channel table, the subscriptions, and the resync decision.
    fn finish_hello(&mut self, mut body: v2xw_record::wire::hello::HelloBody) -> Result<Frame> {
        if self.profile.is_node_only() {
            // §5.2: GT channels are absent from the table entirely, not merely disabled,
            // and `nodes.flags` bit 1 must be zero.
            let gt: BTreeSet<u16> = v2xw_record::CHANNELS
                .iter()
                .filter(|c| c.is_ground_truth_channel())
                .filter_map(|c| c.wire_id)
                .collect();
            body.channels.retain(|c| !gt.contains(&c.channel_id));
            for node in &mut body.nodes {
                node.flags &= !NODE_IS_ATTACKER;
            }
        }
        for row in &mut body.channels {
            row.enabled = u8::from(self.event_channels.contains(&row.channel_id));
        }

        self.greeted = true;
        if !self.resumed {
            // §1.3 rule 2: the first canonical frame after a non-resumed `Hello` is a
            // keyframe with `FLAG_RESYNC` (conformance H2). Two things are needed, and
            // each covers a path the other does not:
            //
            // * `request_keyframe` makes the live encoder emit a keyframe at the next
            //   step even mid-GOP, and that encoder sets `FLAG_RESYNC` itself;
            // * `resync_pending` makes *this* module stamp the flag, which is the only
            //   way the replay path gets it — a recorded frame has its transport bits
            //   cleared (§7.2 item 3), so the stored keyframe carries no `FLAG_RESYNC`
            //   and forwarding it verbatim would open the stream with a keyframe the
            //   client does not know to re-seed from.
            self.encoder.request_keyframe();
            self.resync_pending = true;
        }
        Ok(body.to_frame(body.resume_seq, 0)?)
    }

    /// Marks a resync keyframe as owed (§1.5).
    ///
    /// Called when the transport learns the connection fell behind — a broadcast lag —
    /// rather than when its own queue shed load, which [`Session::encode_step`] notices
    /// for itself.
    pub fn request_resync(&mut self) {
        self.resync_pending = true;
    }

    /// True once `Hello` has gone out.
    pub fn greeted(&self) -> bool {
        self.greeted
    }

    /// True when the handshake resumed an existing stream.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// The retained frames a resumed handshake replays (§1.4 rule 1).
    pub fn resume_backlog(&self) -> Vec<Frame> {
        match self.params.resume.filter(|seq| self.can_resume(*seq)) {
            Some(seq) => self
                .ring
                .replay_from(seq)
                .into_iter()
                .map(|e| e.frame)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Encodes one mobility step into the frames this connection may see.
    ///
    /// The order is the one §1.3's diagram shows: the snapshot frame first, then
    /// provenance (so every `prov_id` is resolvable before it is referenced — conformance
    /// C5), then telemetry, events and metrics.
    ///
    /// # Errors
    /// [`ServerError::Internal`] if the encoder refuses the step, which it does only for a
    /// snapshot that is not on a mobility-step boundary.
    pub fn encode_step(&mut self, out: &StepOutput) -> Result<StepEffects> {
        let mut effects = StepEffects::default();
        if !out.recorded.is_empty() {
            return self.forward_recorded(out);
        }
        // A step the greeting already covered. The greeting encodes the run's state at the
        // instant it is sent, and the kernel can be a step or more past what this
        // connection's step channel has yet delivered; the queued older steps then reach
        // the encoder after a newer snapshot, which it rightly refuses as time going
        // backwards. Refusing it here used to close the socket with 1011 — and every
        // reconnect raced the same way, so a heavy run (Manhattan with 230 pedestrians and
        // cyclists) never streamed. The state those steps carried is in the greeting.
        // Only within this connection's own run: a step from a *new* run (a later generation)
        // against this encoder is the regreet's business, and must still fail loudly rather
        // than be skipped until its clock passes the old run's.
        if out.generation == self.generation()
            && self
                .encoder
                .last_time()
                .is_some_and(|prev| out.snapshot.sim_time <= prev)
        {
            return Ok(effects);
        }
        if self.resync_pending {
            self.encoder.request_keyframe();
        }

        let snapshot = self.encoder.encode(&out.snapshot)?;
        let was_keyframe = snapshot.is_keyframe();
        let seq = self.take_seq();
        let mut frame = snapshot.into_frame().renumbered(seq)?;
        if was_keyframe && self.resync_pending {
            frame = frame.with_flags(frame.header()?.flags | FLAG_RESYNC);
            self.resync_pending = false;
        }
        if out.end_of_run {
            frame = frame.with_flags(frame.header()?.flags | FLAG_END_OF_RUN);
        }
        self.push(frame, was_keyframe, &mut effects);

        // §3.8: at least one `Provenance` frame immediately after the first `Keyframe`,
        // covering every `prov_id` referenced before the next one (conformance C5).
        if was_keyframe {
            if let Some(prov) = self.provenance.take() {
                let frame = self.provenance_frame(&prov)?;
                self.push(frame, false, &mut effects);
            }
        }
        if let Some(prov) = &out.provenance {
            let frame = self.provenance_frame(prov)?;
            self.push(frame, false, &mut effects);
        }
        if let Some(frame) = self.telemetry_frame(out)? {
            self.push(frame, false, &mut effects);
        }
        for frame in self.event_frames(out)? {
            self.push(frame, false, &mut effects);
        }
        if let Some(frame) = self.metric_frame(out)? {
            self.push(frame, false, &mut effects);
        }

        if !self.pending_drops.is_empty() {
            if let Some((first, last)) = self.drop_span.take() {
                effects.drop_notice = Some((first, last, self.pending_drops, self.seq));
                self.pending_drops = DropCounts::default();
            }
        }
        effects.frames = self.drain_queue();
        Ok(effects)
    }

    /// Forwards recorded canonical frames without re-encoding them (§7.2).
    ///
    /// The node profile still applies, and it applies the way §5.3 requires: through
    /// `v2xw-record`'s [`v2xw_record::NodeProfileStripper`], which blanks at the body and
    /// renumbers `seq`, so a `full` recording replayed as `node` is byte-identical to a
    /// live `node` run of the same scenario (conformance V5).
    fn forward_recorded(&mut self, out: &StepOutput) -> Result<StepEffects> {
        let mut effects = StepEffects::default();
        for frame in &out.recorded {
            let frame = match &mut self.stripper {
                Some(stripper) => match stripper.strip(frame)? {
                    Some(f) => f,
                    None => continue,
                },
                None => frame.clone(),
            };
            let header = frame.header()?;
            let keyframe = header.msg_type == MsgType::Keyframe.id();
            let mut frame = frame;
            if keyframe && self.resync_pending {
                frame = frame.with_flags(header.flags | FLAG_RESYNC);
                self.resync_pending = false;
            }
            // `FLAG_END_OF_RUN` is a **canonical** flag (§2.3), not a transport one: the
            // producer set it and the recorder stored it, so setting it here would change
            // a byte inside `CANONICAL_FLAG_MASK` and break §7.2 for the last frame of
            // every run. Live production sets it in `encode_step`, where this crate *is*
            // the producer; replay must leave it exactly as recorded.
            self.seq = frame.header()?.seq.saturating_add(1);
            self.push(frame, keyframe, &mut effects);
        }
        effects.frames = self.drain_queue();
        Ok(effects)
    }

    /// Encodes the answer to a `run.seek`: a `SEEK_RESULT | RESYNC` keyframe and the
    /// deltas after it (§6.6's ordering guarantee, conformance R4 and P4).
    ///
    /// # Errors
    /// As [`Session::encode_step`].
    pub fn encode_seek(&mut self, outputs: &[StepOutput]) -> Result<(Vec<Frame>, u64, usize)> {
        let mut frames = Vec::new();
        let mut keyframe_seq = self.seq;
        let mut deltas = 0usize;
        for (i, out) in outputs.iter().enumerate() {
            if i == 0 {
                // A seek moves simulated time anywhere, including backwards, and the
                // snapshot encoder refuses a step that does not advance — correctly, since
                // a delta against a later base is meaningless. The encoder is therefore
                // rebuilt rather than nudged: the seek keyframe is a full snapshot with
                // `FLAG_RESYNC`, which by §3.3 re-seeds every per-slot reference the
                // encoder held.
                self.encoder =
                    SnapshotEncoder::new(self.origin_m, self.cadence, self.profile, self.seq);
                self.encoder.request_keyframe();
            }
            let snapshot = self.encoder.encode(&out.snapshot)?;
            let is_kf = snapshot.is_keyframe();
            let seq = self.take_seq();
            let mut frame = snapshot.into_frame().renumbered(seq)?;
            if is_kf {
                keyframe_seq = frame.header()?.seq;
                frame = frame.with_flags(frame.header()?.flags | FLAG_RESYNC | FLAG_SEEK_RESULT);
            } else {
                deltas += 1;
            }
            self.ring.push(crate::ring::RingEntry {
                seq: frame.header()?.seq,
                keyframe: is_kf,
                frame: frame.canonical(),
            });
            frames.push(frame);
        }
        // §7.3's optional companion state: telemetry and provenance after a seek carry
        // FLAG_SEEK_RESULT and are outside the byte-identity guarantee.
        if let Some(last) = outputs.last() {
            if let Some(frame) = self.telemetry_frame(last)? {
                let flags = frame.header()?.flags | FLAG_SEEK_RESULT;
                frames.push(frame.with_flags(flags));
            }
        }
        Ok((frames, keyframe_seq, deltas))
    }

    fn push(&mut self, frame: Frame, keyframe: bool, effects: &mut StepEffects) {
        let Ok(header) = frame.header() else {
            effects.fatal = true;
            return;
        };
        let canonical = MsgType::from_id(header.msg_type).is_some_and(MsgType::is_canonical);
        if canonical {
            self.ring.push(crate::ring::RingEntry {
                seq: header.seq,
                keyframe,
                frame: frame.canonical(),
            });
            if let Some(first) = self.ring.first_seq() {
                while self.string_marks.front().is_some_and(|(at, _)| *at < first) {
                    self.string_marks.pop_front();
                }
            }
        }
        let report = self.queue.enqueue(frame);
        self.absorb(report, effects);
    }

    /// Records what an enqueue shed, so the next step can send a `stream.drop` (§1.5).
    fn absorb(&mut self, report: EnqueueReport, effects: &mut StepEffects) {
        if report.fatal {
            effects.fatal = true;
        }
        if report.resync_pending {
            self.resync_pending = true;
        }
        if report.dropped.is_empty() {
            return;
        }
        self.pending_drops.merge(report.dropped);
        if let (Some(first), Some(last)) = (report.seq_first, report.seq_last) {
            self.drop_span = Some(match self.drop_span {
                Some((f, l)) => (f.min(first), l.max(last)),
                None => (first, last),
            });
        }
    }

    /// Drains everything the queue holds, in order.
    pub fn drain_queue(&mut self) -> Vec<Frame> {
        let mut out = Vec::with_capacity(self.queue.len());
        while let Some(frame) = self.queue.pop() {
            out.push(frame);
        }
        out
    }

    /// Queues a P0 frame (`Error`, `Bye`) that must not be dropped.
    ///
    /// # Errors
    /// [`ServerError::Internal`] when even a P0 frame does not fit, which §1.5 turns into
    /// a close with 1011.
    pub fn queue_p0(&mut self, frame: Frame) -> Result<()> {
        let report = self.queue.enqueue(frame);
        if report.fatal {
            return Err(ServerError::Internal(
                "a P0 frame did not fit the send queue".to_string(),
            ));
        }
        Ok(())
    }

    fn provenance_frame(&mut self, prov: &ProvenanceBody) -> Result<Frame> {
        // `Provenance` is META (§5.1): nothing in it is withheld by the node profile.
        let mut body = prov.clone();
        if let Some(ext) = &body.strings {
            // §2.5: the extension's entry `i` takes id `table_size_before + i`, and the
            // table is append-only.
            //
            // The engine numbered those ids against the table in its `RunDescriptor`,
            // which is the table *before* this connection's `Hello` interned its session
            // token and its live node labels. Ids past that base therefore have to be
            // shifted by however much this connection's table has grown, or every
            // `str_model_id` in the frame resolves one entry short and conformance C7
            // fails silently on exactly the connections that carry a token. The shift is
            // computed here, where the actual table size is known, rather than being
            // something the engine has to guess.
            let shift = self.strings.strings.len().saturating_sub(self.hello_base);
            if shift > 0 {
                let base = u32::try_from(self.hello_base).unwrap_or(u32::MAX);
                let by = u32::try_from(shift).unwrap_or(0);
                let bump = |id: &mut u32| {
                    if *id >= base {
                        *id = id.saturating_add(by);
                    }
                };
                for entry in &mut body.entries {
                    bump(&mut entry.str_model_id);
                    bump(&mut entry.str_model_version);
                    bump(&mut entry.str_param_set_id);
                    bump(&mut entry.str_card_url);
                }
                for dim in &mut body.dims {
                    bump(&mut dim.str_dims);
                }
            }
            // Mirroring the append here keeps the session's view of the table exactly what
            // the client will compute.
            if !ext.strings.is_empty() {
                self.string_marks
                    .push_back((self.seq, self.strings.strings.len()));
            }
            self.strings.strings.extend(ext.strings.iter().cloned());
        }
        let seq = self.take_seq();
        Ok(body.to_frame(seq, self.profile.frame_flag())?)
    }

    fn telemetry_frame(&mut self, out: &StepOutput) -> Result<Option<Frame>> {
        if self.telemetry_nodes.is_empty() {
            return Ok(None);
        }
        let mut records: Vec<_> = out
            .telemetry
            .iter()
            .filter(|r| self.telemetry_nodes.contains(&r.node_id))
            .copied()
            .collect();
        if records.is_empty() {
            return Ok(None);
        }
        records.sort_by_key(|r| r.node_id);
        let mut body = TelemetryBody::new(out.sim_time, 1_000_000_000, records);
        if self.profile.is_node_only() {
            v2xw_record::profile::blank_telemetry(&mut body);
        }
        let seq = self.take_seq();
        Ok(Some(body.to_frame(seq, self.profile.frame_flag())?))
    }

    fn event_frames(&mut self, out: &StepOutput) -> Result<Vec<Frame>> {
        if self.event_channels.is_empty() || out.events.is_empty() {
            return Ok(Vec::new());
        }
        let mut kept: Vec<EventEntry> = Vec::new();
        let mut seen = 0usize;
        for entry in &out.events {
            if !self.event_channels.contains(&entry.channel_id) {
                continue;
            }
            if !self.event_nodes.is_empty() {
                let node = entry
                    .payload
                    .get(0..4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_le_bytes);
                // Channel 11's first field is a time, not a node id, so its node filter is
                // applied on `rx_node` at @16 instead.
                let node = if entry.channel_id == 11 {
                    entry
                        .payload
                        .get(16..20)
                        .and_then(|b| b.try_into().ok())
                        .map(u32::from_le_bytes)
                } else {
                    node
                };
                if let Some(node) = node {
                    if !self.event_nodes.contains(&node) {
                        continue;
                    }
                }
            }
            seen += 1;
            if self.sample_1_in > 1 && seen % self.sample_1_in != 0 {
                continue;
            }
            if self.max_events_per_step > 0 && kept.len() >= self.max_events_per_step {
                break;
            }
            let mut entry = entry.clone();
            if self.profile.is_node_only() {
                match v2xw_record::profile::blank_event_payload(
                    entry.channel_id,
                    &mut entry.payload,
                ) {
                    v2xw_record::profile::PayloadVerdict::Drop => continue,
                    v2xw_record::profile::PayloadVerdict::Keep => {}
                }
            }
            kept.push(entry);
        }
        if kept.is_empty() {
            return Ok(Vec::new());
        }
        kept.sort_by_key(|e| (e.sim_time_ns, e.channel_id));
        let body = EventBody::new(out.sim_time, out.sim_time, kept);
        let seq = self.take_seq();
        Ok(vec![body.to_frame(seq, self.profile.frame_flag())?])
    }

    fn metric_frame(&mut self, out: &StepOutput) -> Result<Option<Frame>> {
        if out.metrics.is_empty() {
            return Ok(None);
        }
        let samples: Vec<_> = out
            .metrics
            .iter()
            .filter(|s| !(self.profile.is_node_only() && s.visibility == 0))
            .copied()
            .collect();
        if samples.is_empty() {
            return Ok(None);
        }
        let body = MetricBody::new(out.sim_time, 1_000_000_000, samples);
        let seq = self.take_seq();
        Ok(Some(body.to_frame(seq, self.profile.frame_flag())?))
    }

    // --- subscriptions -----------------------------------------------------------

    /// Applies `view.follow` (§6.7) and returns the resulting subscription list.
    pub fn follow(
        &mut self,
        node: Option<u32>,
        clear: bool,
        telemetry: bool,
        extra: &[u32],
    ) -> Vec<u32> {
        if clear {
            self.following = None;
            self.feed = None;
            self.telemetry_nodes.clear();
            return Vec::new();
        }
        if let Some(node) = node {
            if self.following != Some(node) {
                // The feed follows the node, never outlives it: a feed subscribed for the
                // previous vehicle is not this one's.
                self.feed = None;
            }
            self.following = Some(node);
            if telemetry {
                self.telemetry_nodes.insert(node);
                self.telemetry_nodes.extend(extra.iter().copied());
            }
        }
        self.telemetry_nodes()
    }

    /// Sets the camera (§6.7).
    pub fn set_camera(&mut self, camera: CameraState) {
        self.camera = camera;
    }

    /// Applies `overlay.set` (§6.7).
    ///
    /// # Errors
    /// [`ServerError::VisibilityDenied`] when the `node` profile is asked to enable a
    /// `*_gt` overlay (§5.3, conformance V3).
    pub fn set_overlays(
        &mut self,
        wanted: &BTreeMap<String, bool>,
        opacity: &BTreeMap<String, f64>,
    ) -> Result<()> {
        for (name, on) in wanted {
            if !OVERLAYS.contains(&name.as_str()) {
                return Err(ServerError::param(
                    "/overlays",
                    &format!("unknown overlay `{name}`"),
                    "call overlay.set with {\"list\": true} for the catalogue",
                ));
            }
            if *on && self.profile.is_node_only() && overlay_is_gt(name) {
                return Err(ServerError::VisibilityDenied {
                    field: name.clone(),
                    visibility: "GT",
                });
            }
        }
        for (name, on) in wanted {
            self.overlays.insert(name.clone(), *on);
        }
        for (name, value) in opacity {
            self.opacity.insert(name.clone(), *value);
        }
        Ok(())
    }

    /// Applies `events.set` (§6.12) and returns the subscribed channel ids.
    ///
    /// # Errors
    /// [`ServerError::VisibilityDenied`] for a GT channel in the `node` profile
    /// (conformance V3), or [`ServerError::InvalidParams`] for a channel this server does
    /// not have.
    pub fn set_events(
        &mut self,
        subscribe: &[String],
        unsubscribe: &[String],
        only: Option<&[String]>,
        nodes: Option<&[u32]>,
        sample_1_in: Option<usize>,
        max_events_per_step: Option<usize>,
    ) -> Result<Vec<u16>> {
        let resolve = |name: &str| -> Result<u16> {
            let spec = v2xw_record::channels::by_name(name).ok_or_else(|| {
                ServerError::param(
                    "/subscribe",
                    &format!("unknown channel `{name}`"),
                    "call events.set with {\"list\": true} for the catalogue",
                )
            })?;
            if self.profile.is_node_only() && spec.is_ground_truth_channel() {
                return Err(ServerError::VisibilityDenied {
                    field: name.to_string(),
                    visibility: "GT",
                });
            }
            spec.wire_id.ok_or_else(|| {
                ServerError::param(
                    "/subscribe",
                    &format!("`{name}` is never carried in an Event frame"),
                    "subscribe to a channel with a wire id; see events.set {\"list\": true}",
                )
            })
        };
        if let Some(only) = only {
            let mut set = BTreeSet::new();
            for name in only {
                set.insert(resolve(name)?);
            }
            self.event_channels = set;
        }
        for name in subscribe {
            let id = resolve(name)?;
            self.event_channels.insert(id);
        }
        for name in unsubscribe {
            if let Some(spec) = v2xw_record::channels::by_name(name) {
                if let Some(id) = spec.wire_id {
                    self.event_channels.remove(&id);
                }
            }
        }
        if let Some(nodes) = nodes {
            self.event_nodes = nodes.iter().copied().collect();
        }
        if let Some(n) = sample_1_in {
            self.sample_1_in = n.max(1);
        }
        if let Some(n) = max_events_per_step {
            self.max_events_per_step = n;
        }
        Ok(self.event_channels())
    }
}
