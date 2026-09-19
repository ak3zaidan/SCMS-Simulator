//! `Hello` (§3.1) — the connection preamble, and the one frame a recording keeps as run
//! metadata.
//!
//! `Hello` is not a canonical frame: it carries the seq the next canonical frame will
//! have and consumes none (§2.4), and it is explicitly outside the byte-identity
//! guarantee because `resume_seq`, `sim_time_ns` and `hello_flags` are connection state
//! (§7.2). A recording stores the one produced at run start, which is what tells the
//! replay reader the run's origin, cadence, node table, class table and channel table.

use crate::error::Result;
use crate::wire::{
    Frame, MsgType, StrTable, get_bytes, get_f32, get_f64, get_i64, get_u8, get_u16, get_u32,
    get_u64, put_f32, put_f64, put_i64, put_u8, put_u16, put_u32, put_u64,
};

/// The `Hello` prefix is 256 bytes and fully assigned on purpose (§3.1.1, §8.5).
pub const PREFIX_BYTES: usize = 256;
/// Bytes per node row (§3.1.3).
pub const NODE_STRIDE: usize = 32;
/// Bytes per actor-class row (§3.1.4).
pub const CLASS_STRIDE: usize = 24;
/// Bytes per channel row (§3.1.5).
pub const CHANNEL_STRIDE: usize = 8;
/// The world reference is 16 bytes (§3.1.6).
pub const WORLD_REF_BYTES: usize = 16;

/// `hello_flags` bit 0 — the stream is produced by a live engine (§3.1.2).
pub const HELLO_LIVE: u32 = 0x0000_0001;
/// `hello_flags` bit 1 — the stream is produced by the replay reader (§3.1.2).
pub const HELLO_REPLAY: u32 = 0x0000_0002;
/// `hello_flags` bit 2 — the connection is in the `node` profile (§3.1.2, §5).
pub const HELLO_NODE_ONLY: u32 = 0x0000_0004;
/// `hello_flags` bit 3 — the world arrives as `WorldChunk` frames (§3.1.2).
pub const HELLO_WORLD_INLINE: u32 = 0x0000_0008;
/// `hello_flags` bit 4 — the run exists but is not advancing (§3.1.2).
pub const HELLO_PAUSED: u32 = 0x0000_0010;
/// `hello_flags` bit 5 — this connection resumed an existing stream (§3.1.2).
pub const HELLO_RESUMED: u32 = 0x0000_0020;
/// `hello_flags` bit 6 — `run.seek` is available (§3.1.2).
pub const HELLO_SEEKABLE: u32 = 0x0000_0040;
/// `hello_flags` bit 7 — mutating control methods are permitted (§3.1.2).
pub const HELLO_WRITABLE: u32 = 0x0000_0080;

/// `nodes.flags` bit 0 — the node has a hardware security module (§3.1.3).
pub const NODE_HAS_HSM: u16 = 0x0001;
/// `nodes.flags` bit 1 — the node is an attacker. **Ground truth** (§3.1.3, §5.2).
pub const NODE_IS_ATTACKER: u16 = 0x0002;
/// `nodes.flags` bit 2 — the node is a backend entity (§3.1.3).
pub const NODE_IS_BACKEND: u16 = 0x0004;
/// `nodes.flags` bit 3 — the node has a backhaul link (§3.1.3).
pub const NODE_HAS_BACKHAUL: u16 = 0x0008;
/// `nodes.flags` bit 4 — the node has a Uu (cellular) interface (§3.1.3).
pub const NODE_HAS_UU: u16 = 0x0010;

/// One node-table row (§3.1.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeRow {
    /// The node id, ascending and dense where possible.
    pub node_id: u32,
    /// The actor the node is mounted on, `0xFFFFFFFF` if none.
    pub actor_id: u32,
    /// Position in ENU metres; static nodes are fixed, mobile ones are at `t0`.
    pub pos_m: [f32; 3],
    /// String id of the label, e.g. `"veh_0421"`.
    pub str_label: u32,
    /// String id of the hardware profile, e.g. `"obu/cohda-mk5"`.
    pub str_profile_id: u32,
    /// Node flags; bit 1 is ground truth.
    pub flags: u16,
    /// `NodeKind`: 0 obu, 1 vru-device, 2 rsu, 3 base-station, 4 router, 5 backend, 6 other.
    pub kind: u8,
    /// Index into the class table, `0xFF` if the node is not an actor.
    pub class_idx: u8,
}

/// One actor-class row (§3.1.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassRow {
    /// String id of the class name.
    pub str_name: u32,
    /// Length in metres.
    pub length_m: f32,
    /// Width in metres.
    pub width_m: f32,
    /// Height in metres.
    pub height_m: f32,
    /// Renderer hint, `0xRRGGBBAA`.
    pub color_rgba: u32,
    /// `ActorCategory`: 0 vehicle, 1 vru, 2 infrastructure, 3 other.
    pub category: u8,
}

/// One channel-table row (§3.1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRow {
    /// String id of the channel name, exactly as in 03-interfaces §14.
    pub str_id: u32,
    /// The numeric channel id used in `Event` (§3.6.2).
    pub channel_id: u16,
    /// `Visibility`: 0 GT, 1 NODE, 2 PUBLIC, 3 MIXED, 4 DERIVED, 5 META.
    pub visibility: u8,
    /// `1` if the server will emit it now.
    pub enabled: u8,
}

/// The world reference (§3.1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldRef {
    /// `0` HTTP GET by content hash, `1` inline `WorldChunk`, `2` the client has it.
    pub mode: u8,
    /// `0` `vwp-world/1` binary, `1` JSON.
    pub format: u8,
    /// Total size of the world payload in bytes.
    pub payload_bytes: u32,
    /// String id of the path or URL.
    pub str_url: u32,
}

/// A decoded `Hello` body (§3.1).
#[derive(Debug, Clone, PartialEq)]
pub struct HelloBody {
    /// The protocol minor version.
    pub version_minor: u16,
    /// `hello_flags` (§3.1.2).
    pub hello_flags: u32,
    /// UUIDv7, raw big-endian bytes as in RFC 9562.
    pub run_id: [u8; 16],
    /// SHA-256 of the canonical scenario JSON.
    pub scenario_hash: [u8; 32],
    /// The world's content hash.
    pub world_hash: [u8; 32],
    /// Unix epoch nanoseconds, UTC, of sim time 0.
    pub t0_wall_ns: i64,
    /// The scenario duration in nanoseconds.
    pub sim_duration_ns: u64,
    /// The mobility step in nanoseconds.
    pub mobility_step_ns: u64,
    /// The keyframe period in nanoseconds.
    pub keyframe_period_ns: u64,
    /// The telemetry period in nanoseconds.
    pub telemetry_period_ns: u64,
    /// The metric period in nanoseconds.
    pub metric_period_ns: u64,
    /// The seq of the next canonical frame.
    pub resume_seq: u64,
    /// The current stream position in sim time.
    pub sim_time_ns: u64,
    /// WGS-84 latitude of the ENU origin.
    pub origin_lat_deg: f64,
    /// WGS-84 longitude of the ENU origin.
    pub origin_lon_deg: f64,
    /// Ellipsoidal height of the ENU origin, metres.
    pub origin_alt_m: f64,
    /// The world bounding box in ENU metres: `min_x, min_y, max_x, max_y`.
    pub bbox_m: [f64; 4],
    /// Maximum concurrent actor slots for the run — a preallocation hint.
    pub actor_capacity: u32,
    /// The node table.
    pub nodes: Vec<NodeRow>,
    /// The actor-class table.
    pub classes: Vec<ClassRow>,
    /// The channel table.
    pub channels: Vec<ChannelRow>,
    /// The world reference.
    pub world_ref: WorldRef,
    /// String id of the engine version, e.g. `"v2xw 0.4.0+9f0649d"`.
    pub str_engine_version: u32,
    /// String id of the scenario name.
    pub str_scenario_name: u32,
    /// String id of the human run label.
    pub str_run_label: u32,
    /// String id of the session token.
    pub str_session_token: u32,
    /// The symbol table this `Hello` establishes (§2.5).
    pub strings: StrTable,
}

impl HelloBody {
    /// The encoded body length in bytes (Appendix B).
    pub fn encoded_len(&self) -> usize {
        PREFIX_BYTES
            + NODE_STRIDE * self.nodes.len()
            + CLASS_STRIDE * self.classes.len()
            + CHANNEL_STRIDE * self.channels.len()
            + WORLD_REF_BYTES
            + self.strings.encoded_len()
    }

    /// Encodes the body (§3.1).
    pub fn encode(&self) -> Vec<u8> {
        let n = self.nodes.len();
        let c = self.classes.len();
        let k = self.channels.len();
        let mut out = vec![0u8; self.encoded_len()];
        let mut at = PREFIX_BYTES;
        let off_nodes = if n > 0 { at } else { 0 };
        at += NODE_STRIDE * n;
        let off_classes = if c > 0 { at } else { 0 };
        at += CLASS_STRIDE * c;
        let off_channels = if k > 0 { at } else { 0 };
        at += CHANNEL_STRIDE * k;
        let off_world_ref = at;
        at += WORLD_REF_BYTES;
        let off_strings = at;

        put_u16(&mut out, 0, super::VERSION_MAJOR);
        put_u16(&mut out, 2, self.version_minor);
        put_u32(&mut out, 4, self.hello_flags);
        out[8..24].copy_from_slice(&self.run_id);
        out[24..56].copy_from_slice(&self.scenario_hash);
        out[56..88].copy_from_slice(&self.world_hash);
        put_i64(&mut out, 88, self.t0_wall_ns);
        put_u64(&mut out, 96, self.sim_duration_ns);
        put_u64(&mut out, 104, self.mobility_step_ns);
        put_u64(&mut out, 112, self.keyframe_period_ns);
        put_u64(&mut out, 120, self.telemetry_period_ns);
        put_u64(&mut out, 128, self.metric_period_ns);
        put_u64(&mut out, 136, self.resume_seq);
        put_u64(&mut out, 144, self.sim_time_ns);
        put_f64(&mut out, 152, self.origin_lat_deg);
        put_f64(&mut out, 160, self.origin_lon_deg);
        put_f64(&mut out, 168, self.origin_alt_m);
        put_f64(&mut out, 176, self.bbox_m[0]);
        put_f64(&mut out, 184, self.bbox_m[1]);
        put_f64(&mut out, 192, self.bbox_m[2]);
        put_f64(&mut out, 200, self.bbox_m[3]);
        put_u32(&mut out, 208, self.actor_capacity);
        put_u32(&mut out, 212, n as u32);
        put_u16(&mut out, 216, c as u16);
        put_u16(&mut out, 218, k as u16);
        put_u32(&mut out, 220, off_nodes as u32);
        put_u32(&mut out, 224, off_classes as u32);
        put_u32(&mut out, 228, off_channels as u32);
        put_u32(&mut out, 232, off_world_ref as u32);
        put_u32(&mut out, 236, off_strings as u32);
        put_u32(&mut out, 240, self.str_engine_version);
        put_u32(&mut out, 244, self.str_scenario_name);
        put_u32(&mut out, 248, self.str_run_label);
        put_u32(&mut out, 252, self.str_session_token);

        let mut p = PREFIX_BYTES;
        for r in &self.nodes {
            put_u32(&mut out, p, r.node_id);
            p += 4;
        }
        for r in &self.nodes {
            put_u32(&mut out, p, r.actor_id);
            p += 4;
        }
        for axis in 0..3 {
            for r in &self.nodes {
                put_f32(&mut out, p, r.pos_m[axis]);
                p += 4;
            }
        }
        for r in &self.nodes {
            put_u32(&mut out, p, r.str_label);
            p += 4;
        }
        for r in &self.nodes {
            put_u32(&mut out, p, r.str_profile_id);
            p += 4;
        }
        for r in &self.nodes {
            put_u16(&mut out, p, r.flags);
            p += 2;
        }
        for r in &self.nodes {
            put_u8(&mut out, p, r.kind);
            p += 1;
        }
        for r in &self.nodes {
            put_u8(&mut out, p, r.class_idx);
            p += 1;
        }

        for r in &self.classes {
            put_u32(&mut out, p, r.str_name);
            p += 4;
        }
        for r in &self.classes {
            put_f32(&mut out, p, r.length_m);
            p += 4;
        }
        for r in &self.classes {
            put_f32(&mut out, p, r.width_m);
            p += 4;
        }
        for r in &self.classes {
            put_f32(&mut out, p, r.height_m);
            p += 4;
        }
        for r in &self.classes {
            put_u32(&mut out, p, r.color_rgba);
            p += 4;
        }
        p += 2 * c; // reserved16
        for r in &self.classes {
            put_u8(&mut out, p, r.category);
            p += 1;
        }
        p += c; // reserved8

        for r in &self.channels {
            put_u32(&mut out, p, r.str_id);
            p += 4;
        }
        for r in &self.channels {
            put_u16(&mut out, p, r.channel_id);
            p += 2;
        }
        for r in &self.channels {
            put_u8(&mut out, p, r.visibility);
            p += 1;
        }
        for r in &self.channels {
            put_u8(&mut out, p, r.enabled);
            p += 1;
        }

        put_u8(&mut out, p, self.world_ref.mode);
        put_u8(&mut out, p + 1, self.world_ref.format);
        put_u32(&mut out, p + 4, self.world_ref.payload_bytes);
        put_u32(&mut out, p + 8, self.world_ref.str_url);
        self.strings.encode_into(&mut out, off_strings);
        out
    }

    /// The whole frame. `Hello` is never compressed and carries the next canonical seq
    /// (§1.3 rule 1, §2.4).
    ///
    /// # Errors
    /// [`crate::error::RecordError::Unrepresentable`] for a body larger than `u32::MAX`.
    pub fn to_frame(&self, next_seq: u64, flags: u16) -> Result<Frame> {
        Frame::new(MsgType::Hello, next_seq, flags, &self.encode())
    }

    /// Decodes a body (§3.1).
    ///
    /// # Errors
    /// [`crate::error::RecordError::Truncated`] if a table runs off the end, or
    /// [`crate::error::RecordError::Malformed`] from the symbol table.
    pub fn decode(body: &[u8]) -> Result<Self> {
        const WHAT: &str = "vwp Hello";
        let n = get_u32(body, 212, WHAT)? as usize;
        let c = get_u16(body, 216, WHAT)? as usize;
        let k = get_u16(body, 218, WHAT)? as usize;
        let off_nodes = get_u32(body, 220, WHAT)? as usize;
        let off_classes = get_u32(body, 224, WHAT)? as usize;
        let off_channels = get_u32(body, 228, WHAT)? as usize;
        let off_world_ref = get_u32(body, 232, WHAT)? as usize;
        let off_strings = get_u32(body, 236, WHAT)? as usize;

        let mut nodes = Vec::with_capacity(n);
        for i in 0..n {
            nodes.push(NodeRow {
                node_id: get_u32(body, off_nodes + 4 * i, WHAT)?,
                actor_id: get_u32(body, off_nodes + 4 * n + 4 * i, WHAT)?,
                pos_m: [
                    get_f32(body, off_nodes + 8 * n + 4 * i, WHAT)?,
                    get_f32(body, off_nodes + 12 * n + 4 * i, WHAT)?,
                    get_f32(body, off_nodes + 16 * n + 4 * i, WHAT)?,
                ],
                str_label: get_u32(body, off_nodes + 20 * n + 4 * i, WHAT)?,
                str_profile_id: get_u32(body, off_nodes + 24 * n + 4 * i, WHAT)?,
                flags: get_u16(body, off_nodes + 28 * n + 2 * i, WHAT)?,
                kind: get_u8(body, off_nodes + 30 * n + i, WHAT)?,
                class_idx: get_u8(body, off_nodes + 31 * n + i, WHAT)?,
            });
        }
        let mut classes = Vec::with_capacity(c);
        for i in 0..c {
            classes.push(ClassRow {
                str_name: get_u32(body, off_classes + 4 * i, WHAT)?,
                length_m: get_f32(body, off_classes + 4 * c + 4 * i, WHAT)?,
                width_m: get_f32(body, off_classes + 8 * c + 4 * i, WHAT)?,
                height_m: get_f32(body, off_classes + 12 * c + 4 * i, WHAT)?,
                color_rgba: get_u32(body, off_classes + 16 * c + 4 * i, WHAT)?,
                category: get_u8(body, off_classes + 22 * c + i, WHAT)?,
            });
        }
        let mut channels = Vec::with_capacity(k);
        for i in 0..k {
            channels.push(ChannelRow {
                str_id: get_u32(body, off_channels + 4 * i, WHAT)?,
                channel_id: get_u16(body, off_channels + 4 * k + 2 * i, WHAT)?,
                visibility: get_u8(body, off_channels + 6 * k + i, WHAT)?,
                enabled: get_u8(body, off_channels + 7 * k + i, WHAT)?,
            });
        }
        let world_ref = WorldRef {
            mode: get_u8(body, off_world_ref, WHAT)?,
            format: get_u8(body, off_world_ref + 1, WHAT)?,
            payload_bytes: get_u32(body, off_world_ref + 4, WHAT)?,
            str_url: get_u32(body, off_world_ref + 8, WHAT)?,
        };
        let (strings, _) = StrTable::decode(body, off_strings)?;

        Ok(HelloBody {
            version_minor: get_u16(body, 2, WHAT)?,
            hello_flags: get_u32(body, 4, WHAT)?,
            run_id: get_bytes::<16>(body, 8, WHAT)?,
            scenario_hash: get_bytes::<32>(body, 24, WHAT)?,
            world_hash: get_bytes::<32>(body, 56, WHAT)?,
            t0_wall_ns: get_i64(body, 88, WHAT)?,
            sim_duration_ns: get_u64(body, 96, WHAT)?,
            mobility_step_ns: get_u64(body, 104, WHAT)?,
            keyframe_period_ns: get_u64(body, 112, WHAT)?,
            telemetry_period_ns: get_u64(body, 120, WHAT)?,
            metric_period_ns: get_u64(body, 128, WHAT)?,
            resume_seq: get_u64(body, 136, WHAT)?,
            sim_time_ns: get_u64(body, 144, WHAT)?,
            origin_lat_deg: get_f64(body, 152, WHAT)?,
            origin_lon_deg: get_f64(body, 160, WHAT)?,
            origin_alt_m: get_f64(body, 168, WHAT)?,
            bbox_m: [
                get_f64(body, 176, WHAT)?,
                get_f64(body, 184, WHAT)?,
                get_f64(body, 192, WHAT)?,
                get_f64(body, 200, WHAT)?,
            ],
            actor_capacity: get_u32(body, 208, WHAT)?,
            nodes,
            classes,
            channels,
            world_ref,
            str_engine_version: get_u32(body, 240, WHAT)?,
            str_scenario_name: get_u32(body, 244, WHAT)?,
            str_run_label: get_u32(body, 248, WHAT)?,
            str_session_token: get_u32(body, 252, WHAT)?,
            strings,
        })
    }

    /// The keyframe origin the specification fixes: `floor(bbox_min)` per axis, and zero
    /// for z (§3.3.1 `DECISION`).
    pub fn keyframe_origin(&self) -> [f64; 3] {
        [self.bbox_m[0].floor(), self.bbox_m[1].floor(), 0.0]
    }
}
