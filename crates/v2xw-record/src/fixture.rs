//! Synthetic recordings for tests, benchmarks and the conformance kit.
//!
//! A recording container is hard to test without a run to record, and the engine that
//! produces one lives above this crate. This module is the substitute: a deterministic
//! synthetic run with no randomness in it at all, so two builds of it agree byte for
//! byte, which is exactly the property the tests are about.
//!
//! It is public because `v2xw-server`'s replay tests and the conformance kit need the
//! same fixture, and a second copy of it would drift from this one.
//!
//! # Deterministic by construction
//!
//! The trajectories are closed-form functions of the step index through
//! [`v2xw_core::math`], never the platform libm and never an RNG: a fixture that differed
//! between macOS and Linux would make every byte-identity test a coin toss.

use std::collections::BTreeMap;

use v2xw_core::ids::{ActorId, LaneId, NodeId, SignalId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::{OwnedRecord, Visibility};

use crate::encoder::{ActorPose, Cadence, SignalState, Snapshot, SnapshotEncoder};
use crate::error::Result;
use crate::profile::Profile;
use crate::wire::event::{EventBody, EventEntry};
use crate::wire::hello::{
    ChannelRow, ClassRow, HELLO_LIVE, HELLO_SEEKABLE, HelloBody, NodeRow, WorldRef,
};
use crate::wire::metric::{MetricBody, MetricRow};
use crate::wire::provenance::{PROV_REPLACE_ALL, ProvEntry, ProvenanceBody};
use crate::wire::snapshot::{ST_ATTACKER, ST_EQUIPPED, ST_TRANSMITTING};
use crate::wire::telemetry::{NodeTelemetry, TelemetryBody};
use crate::wire::{Frame, StrTable, U32_NONE};
use crate::writer::{RecordingOptions, RecordingSummary, RecordingWriter};

/// The shape of a synthetic run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunShape {
    /// How many actors are on the road.
    pub actors: u32,
    /// How many mobility steps to produce.
    pub steps: u32,
    /// How many signal heads.
    pub signals: u32,
    /// The cadence.
    pub cadence: Cadence,
    /// Which stream to produce.
    pub profile: Profile,
    /// The step at which one actor teleports far enough to need the absolute escape of
    /// §3.2; `None` for a run with no teleport.
    pub teleport_at: Option<u32>,
    /// Adds one **parked attacker** in the slot above the others.
    ///
    /// It is equipped, its quantised pose, heading and speed never change, it changes lane
    /// at step 3 and its `ST_ATTACKER` bit is set from step 6. Both of those are §5.2
    /// ground truth, so a live `node`-profile producer emits *no* moved row for it at
    /// either step while a `full` producer emits one at both — which is the only shape of
    /// run in which the `NODE-only` stripper and the live blind producer can disagree
    /// (conformance V5), and the shape in which the surviving all-zero row would announce
    /// that a withheld field changed. Every earlier fixture moved every actor every step,
    /// so it agreed with the implementation instead of testing it.
    pub parked_attacker: bool,
}

impl Default for RunShape {
    fn default() -> Self {
        RunShape {
            actors: 8,
            steps: 40,
            signals: 2,
            cadence: Cadence::DEFAULT,
            profile: Profile::Full,
            teleport_at: None,
            parked_attacker: false,
        }
    }
}

impl RunShape {
    /// A run of `actors` actors over `steps` steps, everything else defaulted.
    pub fn new(actors: u32, steps: u32) -> Self {
        RunShape {
            actors,
            steps,
            ..Default::default()
        }
    }

    /// The same shape in the `NODE-only` profile.
    pub fn node_only(mut self) -> Self {
        self.profile = Profile::NodeOnly;
        self
    }

    /// The same shape with the parked attacker of [`RunShape::parked_attacker`].
    pub fn with_parked_attacker(mut self) -> Self {
        self.parked_attacker = true;
        self
    }

    /// The quantisation origin the fixture uses: `floor(bbox_min)` per axis, `0` for z
    /// (§3.3.1).
    pub fn origin(&self) -> [f64; 3] {
        [-500.0, -500.0, 0.0]
    }
}

/// The snapshot sequence of a synthetic run.
///
/// Actor `i` drives east at `12 + i/4` m/s along a sinusoidal lateral offset, so every
/// step moves it by a non-integral number of millimetres — which is what makes the drift
/// test meaningful. Actor 0 changes lane every 20 steps, actor 1 is an unequipped
/// pedestrian (so the `NODE-only` profile has something to drop), and the last actor
/// spawns at step 5 and despawns at step 25 when the run is long enough.
///
/// With [`RunShape::parked_attacker`] there is one more, in slot `actors`, that never
/// moves and whose only changes are ground truth.
pub fn snapshots(shape: &RunShape) -> Vec<Snapshot> {
    let step_ns = shape.cadence.mobility_step.as_nanos();
    let mut out = Vec::with_capacity(shape.steps as usize);
    for k in 0..shape.steps {
        let t: SimTime = u64::from(k) * step_ns;
        let secs = (k as f64) * (step_ns as f64) / 1e9;
        let mut actors = Vec::with_capacity(shape.actors as usize);
        for i in 0..shape.actors {
            let late = shape.actors > 2 && i == shape.actors - 1;
            if late && !(5..25).contains(&k) {
                continue;
            }
            let speed = 12.0 + f64::from(i) / 4.0;
            let lateral = 3.5 * v2xw_core::math::sin(0.35 * secs + f64::from(i));
            let mut x = -480.0 + f64::from(i) * 7.5 + speed * secs;
            if shape.teleport_at == Some(k) && i == 0 {
                // Far enough to blow past the 32,000 mm delta range (§3.2's escape).
                x += 250.0;
            }
            let equipped = i != 1;
            actors.push(ActorPose {
                slot: i,
                actor: ActorId::new(i),
                node: equipped.then(|| NodeId::new(i)),
                pos_m: [x, lateral, 0.15 + f64::from(i % 3) * 0.01],
                heading_rad: 0.02 * secs * f64::from(i + 1),
                speed_mps: speed,
                accel_mps2: 0.5 * v2xw_core::math::cos(0.2 * secs + f64::from(i)),
                lane: Some(LaneId::new(40 + (k / 20) % 3 + i)),
                class_idx: (i % 3) as u8,
                state: if equipped {
                    ST_EQUIPPED | ST_TRANSMITTING
                } else {
                    0
                },
                verified_neighbors: (i % 16) as u8,
            });
        }
        if shape.parked_attacker {
            // Chosen so every quantised field is constant: x and y are whole millimetres
            // about the origin, z a whole centimetre, heading and speed exactly zero. Only
            // the lane and the attacker bit ever change, and both are ground truth.
            actors.push(ActorPose {
                slot: shape.actors,
                actor: ActorId::new(shape.actors),
                node: Some(NodeId::new(shape.actors)),
                pos_m: [100.0, -50.0, 0.25],
                heading_rad: 0.0,
                speed_mps: 0.0,
                accel_mps2: 0.0,
                lane: Some(LaneId::new(if k < 3 { 70 } else { 71 })),
                class_idx: 0,
                state: ST_EQUIPPED | ST_TRANSMITTING | if k >= 6 { ST_ATTACKER } else { 0 },
                verified_neighbors: 3,
            });
        }
        let signals = (0..shape.signals)
            .map(|s| SignalState {
                signal: SignalId::new(s),
                phase: (((k / 5) + s) % 10) as u8,
                time_to_change: Some(Duration::from_millis(u64::from(128 - (k % 100)) * 100)),
            })
            .collect();
        let mut spawn_causes = BTreeMap::new();
        let mut despawn_causes = BTreeMap::new();
        if shape.actors > 2 {
            spawn_causes.insert(shape.actors - 1, 0);
            despawn_causes.insert(shape.actors - 1, 1);
        }
        out.push(Snapshot {
            sim_time: t,
            actors,
            signals,
            spawn_causes,
            despawn_causes,
        });
    }
    out
}

/// The `Hello` a synthetic run opens with.
pub fn hello(shape: &RunShape) -> HelloBody {
    let mut strings = StrTable::new();
    let engine = strings.intern("v2xw 0.1.0+fixture");
    let scenario = strings.intern("fixture");
    let label = strings.intern("synthetic run");
    let token = strings.intern("");
    let url = strings
        .intern("/world/0000000000000000000000000000000000000000000000000000000000000000.vwb");
    let nodes = (0..shape.actors.min(4))
        .map(|i| NodeRow {
            node_id: i,
            actor_id: i,
            pos_m: [0.0, 0.0, 0.0],
            str_label: strings.intern(&format!("veh_{i:04}")),
            str_profile_id: strings.intern("obu/cohda-mk5"),
            flags: crate::wire::hello::NODE_HAS_HSM,
            kind: 0,
            class_idx: (i % 3) as u8,
        })
        .collect();
    let classes = vec![
        ClassRow {
            str_name: strings.intern("car"),
            length_m: 4.5,
            width_m: 1.8,
            height_m: 1.5,
            color_rgba: 0x3b82_f6ff,
            category: 0,
        },
        ClassRow {
            str_name: strings.intern("truck"),
            length_m: 12.0,
            width_m: 2.55,
            height_m: 3.6,
            color_rgba: 0xf59e_0bff,
            category: 0,
        },
        ClassRow {
            str_name: strings.intern("pedestrian"),
            length_m: 0.5,
            width_m: 0.5,
            height_m: 1.75,
            color_rgba: 0x10b9_81ff,
            category: 1,
        },
    ];
    let mut channels = Vec::new();
    for spec in crate::channels::CHANNELS {
        if shape.profile.is_node_only() && spec.visibility == Visibility::Gt {
            continue;
        }
        let Some(wire_id) = spec.wire_id else {
            continue;
        };
        channels.push(ChannelRow {
            str_id: strings.intern(spec.name),
            channel_id: wire_id,
            visibility: visibility_code(spec.visibility),
            enabled: 1,
        });
    }
    let origin = shape.origin();
    let mut flags = HELLO_LIVE | HELLO_SEEKABLE;
    if shape.profile.is_node_only() {
        flags |= crate::wire::hello::HELLO_NODE_ONLY;
    }
    HelloBody {
        version_minor: crate::wire::VERSION_MINOR,
        hello_flags: flags,
        run_id: [
            0x01, 0x89, 0xd4, 0xc7, 0x9f, 0x3a, 0x7b, 0x21, 0x8e, 0x44, 0x5c, 0x6d, 0x7e, 0x8f,
            0x9a, 0x0b,
        ],
        scenario_hash: [0x11; 32],
        world_hash: [0x22; 32],
        t0_wall_ns: 1_804_143_600_000_000_000,
        sim_duration_ns: u64::from(shape.steps) * shape.cadence.mobility_step.as_nanos(),
        mobility_step_ns: shape.cadence.mobility_step.as_nanos(),
        keyframe_period_ns: shape.cadence.keyframe_period.as_nanos(),
        telemetry_period_ns: shape.cadence.keyframe_period.as_nanos(),
        metric_period_ns: shape.cadence.keyframe_period.as_nanos(),
        resume_seq: 0,
        sim_time_ns: 0,
        origin_lat_deg: 52.516_3,
        origin_lon_deg: 13.377_7,
        origin_alt_m: 34.0,
        bbox_m: [origin[0], origin[1], 500.0, 500.0],
        actor_capacity: shape.actors.next_power_of_two(),
        nodes,
        classes,
        channels,
        world_ref: WorldRef {
            mode: 0,
            format: 0,
            payload_bytes: 0,
            str_url: url,
        },
        str_engine_version: engine,
        str_scenario_name: scenario,
        str_run_label: label,
        str_session_token: token,
        strings,
    }
}

/// The `Visibility` code of §3.1.5 for a core visibility tag.
pub fn visibility_code(v: Visibility) -> u8 {
    match v {
        Visibility::Gt => 0,
        Visibility::Node => 1,
        Visibility::Public => 2,
        Visibility::NodeAndGt | Visibility::Mixed => 3,
        Visibility::Derived => 4,
        Visibility::Meta => 5,
        // `Visibility` is `#[non_exhaustive]`; a tag this build does not know is reported
        // as META rather than guessed at, because guessing GT or NODE would be a leak or
        // a loss.
        _ => 5,
    }
}

/// The complete frame stream of a synthetic run, in canonical order: the `Hello`, then
/// per step a snapshot frame, a `Provenance` frame before the first `MetricSample`, and at
/// each keyframe boundary a `Telemetry` frame, a `MetricSample` frame and one
/// single-channel `Event` frame per event channel.
///
/// The `Provenance` frame is not decoration. Conformance C5 requires "every `prov_id`
/// referenced by a `MetricSample` or an event payload to have been delivered in a
/// `Provenance` frame before it is first referenced", and [`metric_frame`] references
/// `prov_id` 1 and 2 — so a fixture without it modelled a non-conformant producer, and is
/// the fixture the conformance kit and `v2xw-server`'s replay tests are told to reuse.
/// [`crate::reader::Reader::verify`] now checks C5 for metric samples.
///
/// The `Event` batches are single-channel on purpose. §7.1 has the recorder split a mixed
/// batch by channel so that per-channel message indexes work, and a split is a re-encode
/// — the one case in which a stored frame is not the frame that went over the wire. A
/// fixture whose batches were mixed would therefore make the byte-identity test test the
/// splitter instead of the guarantee; the splitter has its own test.
///
/// # Errors
/// Whatever the encoders return; in practice only a body larger than `u32::MAX`.
pub fn live_frames(shape: &RunShape) -> Result<Vec<Frame>> {
    let mut encoder = SnapshotEncoder::new(shape.origin(), shape.cadence, shape.profile, 0);
    let flag = shape.profile.frame_flag();
    let mut frames = Vec::new();
    // `Hello` carries the seq the next canonical frame will have and consumes none (§2.4);
    // `renumber` assigns every seq at the end.
    frames.push(hello(shape).to_frame(0, 0)?);
    let mut provenance_delivered = false;
    for snap in snapshots(shape) {
        let frame = encoder.encode(&snap)?.into_frame();
        let is_keyframe = frame.header()?.kind() == Some(crate::wire::MsgType::Keyframe);
        frames.push(frame);
        if !is_keyframe {
            continue;
        }
        if !provenance_delivered {
            // C5: before the first reference, not merely somewhere in the file.
            frames.push(provenance_frame(shape, snap.sim_time)?.with_flags(flag));
            provenance_delivered = true;
        }
        frames.push(telemetry_frame(shape, snap.sim_time)?.with_flags(flag));
        frames.push(metric_frame(shape, snap.sim_time)?.with_flags(flag));
        for event in event_frames(shape, snap.sim_time)? {
            frames.push(event.with_flags(flag));
        }
    }
    renumber(&mut frames)?;
    Ok(frames)
}

/// One `Telemetry` frame for the first few nodes (§3.5).
///
/// # Errors
/// As [`live_frames`].
pub fn telemetry_frame(shape: &RunShape, at: SimTime) -> Result<Frame> {
    let body = TelemetryBody::new(
        at,
        shape.cadence.keyframe_period.as_nanos(),
        (0..shape.actors.min(4))
            .map(|i| {
                let mut t = NodeTelemetry::unknown(i);
                t.msgs_out_per_s = 10.0;
                t.msgs_in_per_s = 37.5;
                t.verify_wait_p95_ms = 1.437_5;
                t.cbr_pm = 120 + (i as u16);
                t.gnss_hdop = 0.813_7;
                t.gnss_sigma_m = 1.234_5;
                t.pos_error_m = 0.456_789;
                t.clock_offset_ns = 1_234;
                t.node_state = if i == 1 { 6 } else { 2 };
                t.gnss_fix = 2;
                t.verify_policy = 0;
                t
            })
            .collect(),
    );
    let mut body = body;
    if shape.profile.is_node_only() {
        // §5.3: blanking happens at the producer, before serialisation.
        crate::profile::blank_telemetry(&mut body);
    }
    body.to_frame(0, 0)
}

/// The `Provenance` frame that delivers the `prov_id`s [`metric_frame`] references (§3.8,
/// conformance C5).
///
/// The same frame in both profiles: a model id, a version and a parameter set are not
/// ground truth — §5.2 does not list them and the `why` panel has to work in a blind
/// demonstration — so the `NODE-only` stripper passes it through unchanged and V5 holds.
///
/// # Errors
/// As [`live_frames`].
pub fn provenance_frame(_shape: &RunShape, at: SimTime) -> Result<Frame> {
    let mut strings = StrTable::new();
    let model_pdr = strings.intern("metric/pdr");
    let model_ttc = strings.intern("metric/ttc");
    let version = strings.intern("0.1.0");
    let params = strings.intern("sha256:0000");
    let card = strings.intern("/cards/fixture.md");
    let body = ProvenanceBody {
        sim_time_ns: at,
        entries: vec![
            ProvEntry {
                prov_id: 1,
                str_model_id: model_pdr,
                str_model_version: version,
                str_param_set_id: params,
                str_card_url: card,
                family: 0,
                // 4 = metric (§3.8).
                subject_kind: 4,
            },
            ProvEntry {
                prov_id: 2,
                str_model_id: model_ttc,
                str_model_version: version,
                str_param_set_id: params,
                str_card_url: card,
                family: 0,
                subject_kind: 4,
            },
        ],
        dims: Vec::new(),
        strings: Some(strings),
        flags: PROV_REPLACE_ALL,
    };
    body.to_frame(0, 0)
}

/// One `MetricSample` frame with a derived and a ground-truth sample (§3.7).
///
/// # Errors
/// As [`live_frames`].
pub fn metric_frame(shape: &RunShape, at: SimTime) -> Result<Frame> {
    let mut strings = StrTable::new();
    let pdr = strings.intern("pdr");
    let ttc = strings.intern("ttc_min");
    let mut samples = vec![MetricRow {
        value: 0.987_654_3,
        str_metric: pdr,
        dim_key: 0,
        node_id: U32_NONE,
        count: 1_000,
        agg: 5,
        visibility: 4,
        prov_id: 1,
    }];
    if !shape.profile.is_node_only() {
        // visibility 0 is GT, which the node profile does not emit (§5.2).
        samples.push(MetricRow {
            // A ground-truth metric value; the digits are arbitrary.
            value: 3.125_75,
            str_metric: ttc,
            dim_key: 0,
            node_id: U32_NONE,
            count: 12,
            agg: 8,
            visibility: 0,
            prov_id: 2,
        });
    }
    MetricBody::new(at, shape.cadence.keyframe_period.as_nanos(), samples).to_frame(0, 0)
}

/// One single-channel `Event` frame per event channel the profile allows (§3.6).
///
/// The payloads are the fixed-size records of §3.6.4, §3.6.5 and §3.6.11, filled enough
/// that the `NODE-only` blanking has something to blank.
///
/// # Errors
/// As [`live_frames`].
pub fn event_frames(shape: &RunShape, at: SimTime) -> Result<Vec<Frame>> {
    let mut out = Vec::new();
    // node.tx (channel 10, NODE), 40 bytes.
    let mut tx = vec![0u8; 40];
    crate::wire::put_u32(&mut tx, 0, 1);
    crate::wire::put_u32(&mut tx, 4, (at / 1_000_000) as u32);
    crate::wire::put_u32(&mut tx, 8, 352);
    crate::wire::put_f32(&mut tx, 12, 0.578_125);
    crate::wire::put_u16(&mut tx, 16, 2);
    crate::wire::put_i16(&mut tx, 18, 2_000);
    out.push(EventBody::new(
        at,
        at,
        vec![EventEntry {
            sim_time_ns: at,
            channel_id: 10,
            payload: tx,
        }],
    ));
    // phy.rx (channel 11, MIXED): tx_node, distance_m and los_class are ground truth.
    let mut rx = vec![0u8; 48];
    crate::wire::put_u64(&mut rx, 0, at);
    crate::wire::put_u64(&mut rx, 8, at + 200_000);
    crate::wire::put_u32(&mut rx, 16, 2);
    crate::wire::put_u32(&mut rx, 20, 1);
    crate::wire::put_u32(&mut rx, 24, (at / 1_000_000) as u32);
    crate::wire::put_f32(&mut rx, 28, -78.25);
    crate::wire::put_f32(&mut rx, 32, 12.5);
    crate::wire::put_f32(&mut rx, 36, 123.5);
    crate::wire::put_u8(&mut rx, 42, 1);
    out.push(EventBody::new(
        at,
        at,
        vec![EventEntry {
            sim_time_ns: at,
            channel_id: 11,
            payload: rx,
        }],
    ));
    if !shape.profile.is_node_only() {
        // gt.kinematics (channel 1, GT), 56 bytes — a whole channel the node profile drops.
        let mut gt = vec![0u8; 56];
        crate::wire::put_u32(&mut gt, 0, 0);
        crate::wire::put_u32(&mut gt, 4, 42);
        crate::wire::put_f32(&mut gt, 8, 12.345);
        crate::wire::put_f32(&mut gt, 12, -3.21);
        out.push(EventBody::new(
            at,
            at,
            vec![EventEntry {
                sim_time_ns: at,
                channel_id: 1,
                payload: gt,
            }],
        ));
    }
    if shape.profile.is_node_only() {
        // §5.3 again: the producer blanks the ground-truth columns of a mixed payload
        // before it goes on the wire.
        for body in &mut out {
            for e in &mut body.entries {
                let _ = crate::profile::blank_event_payload(e.channel_id, &mut e.payload);
            }
        }
    }
    out.into_iter().map(|b| b.to_frame(0, 0)).collect()
}

/// Rewrites the canonical `seq` values so they are dense in emission order, and so each
/// `Hello` carries the seq of the frame that follows it (§1.4, §2.4, conformance H4).
fn renumber(frames: &mut [Frame]) -> Result<()> {
    let mut seq = 0u64;
    for frame in frames.iter_mut() {
        let h = frame.header()?;
        let kind = h.kind();
        if kind.is_some_and(crate::wire::MsgType::is_canonical) {
            let mut bytes = frame.as_bytes().to_vec();
            crate::wire::put_u64(&mut bytes, 16, seq);
            *frame = Frame::from_bytes(bytes)?;
            seq += 1;
        } else {
            let mut bytes = frame.as_bytes().to_vec();
            crate::wire::put_u64(&mut bytes, 16, seq);
            *frame = Frame::from_bytes(bytes)?;
        }
    }
    Ok(())
}

/// A handful of serde records on the channels the exporters are tested against.
///
/// Every float is already on its field's declared grid, because the recording is itself a
/// recorded artefact and D9 forbids a raw double in one: these seven fields used to be
/// written raw and hashed raw by `content_digest`, and the exporters' quantisation hid it
/// from the grid scan. [`crate::writer::RecordingWriter::write_record`] now refuses an
/// off-grid record, so a fixture that drifted off the grid fails the test that uses it.
pub fn records(shape: &RunShape) -> Vec<(SimTime, OwnedRecord)> {
    let step_ns = shape.cadence.mobility_step.as_nanos();
    let mut out = Vec::new();
    for k in 0..shape.steps {
        let t = u64::from(k) * step_ns;
        let i = k % shape.actors.max(1);
        out.push((
            t,
            OwnedRecord {
                channel: "node.tx",
                visibility: Visibility::Node,
                json: serde_json::to_vec(&serde_json::json!({
                    "node_id": i,
                    "msg_id": k,
                    "bytes_on_air": 320 + i,
                    // Every float here is on the grid its name declares (D9): `_ms` and
                    // `_m` on 1e-3, `_db`/`_dbm` on 1e-2, `cbr` on 1e-4, an unnamed unit
                    // on 1e-6. `write_record` refuses anything else.
                    "airtime_ms": 0.526,
                    "msg_type": 2,
                    "tx_power_cdbm": 2000,
                }))
                .unwrap_or_default(),
            },
        ));
        out.push((
            t,
            OwnedRecord {
                channel: "phy.rx",
                visibility: Visibility::NodeAndGt,
                json: serde_json::to_vec(&serde_json::json!({
                    "rx_node": i,
                    "tx_node": (i + 1) % shape.actors.max(1),
                    "msg_id": k,
                    "rssi_dbm": -78.12,
                    "sinr_db": 12.99,
                    "distance_m": 123.457,
                    "outcome": 0,
                    "los_class": 1,
                }))
                .unwrap_or_default(),
            },
        ));
        out.push((
            t,
            OwnedRecord {
                channel: "mac.cbr",
                visibility: Visibility::Node,
                json: serde_json::to_vec(&serde_json::json!({
                    "node_id": i,
                    "cbr": 0.123_5,
                    "channel": 180,
                }))
                .unwrap_or_default(),
            },
        ));
        if k % 10 == 0 {
            out.push((
                t,
                OwnedRecord {
                    channel: "det.observation",
                    visibility: Visibility::Node,
                    json: serde_json::to_vec(&serde_json::json!({
                        "node_id": i,
                        "detector": "plausibility/speed",
                        "score": 0.876_543,
                        "subject_actor_id": (i + 2) % shape.actors.max(1),
                        "evidence_count": 4,
                    }))
                    .unwrap_or_default(),
                },
            ));
            out.push((
                t,
                OwnedRecord {
                    channel: "metric.sample",
                    visibility: Visibility::Derived,
                    json: serde_json::to_vec(&serde_json::json!({
                        "metric": "pdr",
                        "value": 0.912_346,
                        "count": 500,
                        "agg": "ratio",
                    }))
                    .unwrap_or_default(),
                },
            ));
        }
    }
    out
}

/// Writes a synthetic recording and returns the frames that went into it.
///
/// The returned frames are the *live* stream: comparing them with
/// [`crate::reader::Reader::replay`]'s output is the byte-identity test of §7.2.
///
/// # Errors
/// Whatever the encoders, the container or the file system return.
pub fn write_recording(
    path: impl AsRef<std::path::Path>,
    shape: &RunShape,
) -> Result<(Vec<Frame>, RecordingSummary)> {
    write_recording_with(
        path,
        shape,
        RecordingOptions {
            cadence: shape.cadence,
            profile: shape.profile,
            ..Default::default()
        },
        true,
    )
}

/// [`write_recording`] with explicit container options, and a switch for the serde
/// records.
///
/// # Errors
/// As [`write_recording`].
pub fn write_recording_with(
    path: impl AsRef<std::path::Path>,
    shape: &RunShape,
    opts: RecordingOptions,
    with_records: bool,
) -> Result<(Vec<Frame>, RecordingSummary)> {
    let frames = live_frames(shape)?;
    let mut writer = RecordingWriter::create(path, opts)?;
    writer.write_manifest(
        r#"{"schema":"v2xw/manifest/1","run":"fixture","engine":"v2xw 0.1.0+fixture"}"#,
    )?;
    for frame in &frames {
        writer.write_frame(frame)?;
    }
    if with_records {
        for (at, record) in records(shape) {
            if shape.profile.is_node_only() && record.visibility.is_gt_tainted() {
                continue;
            }
            writer.write_record(at, &record)?;
        }
    }
    writer.attach("scenario.yaml", "application/yaml", b"name: fixture\n")?;
    let summary = writer.finish()?;
    Ok((frames, summary))
}

/// A unique scratch directory for a test or a benchmark.
///
/// Under the system temporary directory, named for the process and a caller-supplied tag,
/// so two tests running in parallel cannot collide.
///
/// # Errors
/// [`crate::error::RecordError::Io`] if the directory cannot be created.
pub fn scratch_dir(tag: &str) -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("v2xw-record-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| crate::error::RecordError::io(&dir, e))?;
    Ok(dir)
}
