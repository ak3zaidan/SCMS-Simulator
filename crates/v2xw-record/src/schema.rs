//! The self-describing schema records — §7.1: "`data` = the UTF-8 bytes of the relevant
//! section of this document's layout tables (so the file is self-describing)".
//!
//! A recording that outlives this crate has to be readable from itself. Each `vwp/…`
//! channel therefore carries the layout of the frame it holds, as the specification's own
//! table, so someone with the file and nothing else can decode it. Each `record/…`
//! channel carries a JSON Schema naming the channel, its visibility and the grid every
//! float on it is quantised to (D9), which is also what the exporters' `schema.json`
//! sidecar repeats.

use crate::channels::ChannelSpec;
use crate::wire::MsgType;

/// The frame-header layout (§2.1), prepended to every frame channel's schema because
/// every message on those channels begins with it.
pub const HEADER_LAYOUT: &str = "\
vwp.v1 frame header (docs/protocol/vwp-v1.md §2.1), little-endian, 24 bytes:
  @0  u32 magic      = 0x31505756 (wire bytes 'V' 'W' 'P' '1')
  @4  u16 version    = protocol major
  @6  u16 msg_type
  @8  u32 body_len   = uncompressed body length
  @12 u16 flags      (§2.3; canonical mask 0x000C, transport mask 0x0033)
  @14 u16 reserved   = 0
  @16 u64 seq        canonical sequence number
  body starts at frame offset 24
";

const KEYFRAME_LAYOUT: &str = "\
vwp.v1.Keyframe body (§3.3), prefix 64 bytes then struct-of-array blocks:
  @0  u64 sim_time_ns          @8  f64 origin_x_m    @16 f64 origin_y_m   @24 f64 origin_z_m
  @32 u32 actor_count (A)      @36 u32 signal_count (S)
  @40 u32 off_actors           @44 u32 off_signals   @48 u32 gop_index
  @52 u16 profile (0 full, 1 node-only)              @54 u16 reserved = 0
  @56 u64 reserved64 = 0
actor block, 28*A bytes, columns in order, each of length A:
  u32 actor_id (0xFFFFFFFF = empty slot), i32 x_mm, i32 y_mm, u32 lane_id [GT],
  i16 z_cm, u16 heading_brad, i16 speed_cq (1/128 m/s), i16 accel_cq (1/64 m/s2) [GT],
  u8 class_idx, u8 state (§3.3.4), u8 verified_neighbors, u8 flags8 = 0
signal block, 8*S bytes: u32 signal_id, u16 time_to_change_ds, u8 phase, u8 reserved = 0
quantisation (§3.2): x/y are millimetres from origin, z centimetres, heading binary
radians (rad = brad*2pi/65536); rows are indexed by slot, not by actor id.
";

const DELTA_LAYOUT: &str = "\
vwp.v1.Delta body (§3.4), prefix 64 bytes then six blocks:
  @0  u64 sim_time_ns   @8  u32 gop_index   @12 u32 step_index (1-based in the GOP)
  @16 u32 moved_count (M)   @20 u32 abs_count   @24 u32 lane_count
  @28 u32 spawn_count (P)   @32 u32 despawn_count (D)   @36 u32 signal_count (S)
  @40 u32 off_moved @44 off_abs @48 off_lanes @52 off_spawns @56 off_despawns @60 off_signals
moved block, 20*M bytes: u32 slot (ascending), i16 dx_mm, i16 dy_mm, i16 dz_mm,
  u16 heading_brad (absolute), i16 speed_cq (absolute), i16 accel_cq (absolute) [GT],
  u8 state, u8 verified_neighbors, u8 mflags (1 ABSOLUTE, 2 LANE_CHANGED), u8 reserved
absolute block, 12*abs_count bytes, array of structs: i32 x_mm, i32 y_mm, i16 z_cm, u16 rsvd
lane block, 4*lane_count bytes [GT]: u32 lane_id, in moved-row order
spawn block, 36*P bytes: u32 slot, u32 actor_id, u32 node_id, i32 x_mm, i32 y_mm,
  u32 lane_id [GT], i16 z_cm, u16 heading_brad, i16 speed_cq, u16 cause [GT],
  u8 class_idx, u8 state, u8 verified_neighbors, u8 reserved
despawn block, 8*D bytes: u32 slot, u16 cause [GT], u16 reserved
signal block, 8*S bytes: as vwp.v1.Keyframe
dx/dy/dz are millimetres against the previously TRANSMITTED quantised value (§3.2), so
quantisation error does not accumulate; a step over 32000 mm sets MFLAG_ABSOLUTE and
appears in the absolute block instead. z's transmitted reference is millimetres and a
keyframe or an escape sets it to z_cm*10.
";

const HELLO_LAYOUT: &str = "\
vwp.v1.Hello body (§3.1): prefix 256 bytes, node table (32*N), class table (24*C),
channel table (8*K), world ref (16), symbol table (§2.5).
prefix: @0 u16 version_major, @2 u16 version_minor, @4 u32 hello_flags,
  @8 u8[16] run_id, @24 u8[32] scenario_hash, @56 u8[32] world_hash,
  @88 i64 t0_wall_ns, @96 u64 sim_duration_ns, @104 u64 mobility_step_ns,
  @112 u64 keyframe_period_ns, @120 u64 telemetry_period_ns, @128 u64 metric_period_ns,
  @136 u64 resume_seq, @144 u64 sim_time_ns, @152 f64 origin_lat_deg,
  @160 f64 origin_lon_deg, @168 f64 origin_alt_m, @176..208 f64 bbox min_x min_y max_x max_y,
  @208 u32 actor_capacity, @212 u32 node_count, @216 u16 class_count, @218 u16 channel_count,
  @220 u32 off_nodes, @224 off_classes, @228 off_channels, @232 off_world_ref, @236 off_strings,
  @240 u32 str_engine_version, @244 str_scenario_name, @248 str_run_label, @252 str_session_token
node table columns: u32 node_id, u32 actor_id, f32 pos_x_m, f32 pos_y_m, f32 pos_z_m,
  u32 str_label, u32 str_profile_id, u16 flags (bit1 IS_ATTACKER [GT]), u8 kind, u8 class_idx
class table columns: u32 str_name, f32 length_m, f32 width_m, f32 height_m,
  u32 color_rgba, u16 reserved16, u8 category, u8 reserved8
channel table columns: u32 str_id, u16 channel_id, u8 visibility, u8 enabled
world ref: u8 mode, u8 format, u16 reserved, u32 payload_bytes, u32 str_url, u32 reserved32
symbol table: u32 n, u32 blob_bytes, u32[n+1] offsets, UTF-8 blob padded to 4
";

const TELEMETRY_LAYOUT: &str = "\
vwp.v1.Telemetry body (§3.5): prefix 32 bytes then `node_count` records of `record_size`
bytes each (208 in v1). Readers MUST stride by the wire record_size, not by 208.
prefix: @0 u64 sim_time_ns (window end), @8 u64 window_ns, @16 u32 node_count,
  @20 u32 off_records (8-aligned), @24 u32 record_size, @28 u32 reserved = 0
record (§3.5.2): @0 u64 storage_used_b, @8 storage_total_b, @16 next_topup_ns,
  @24 crl_bytes, @32 outbox_bytes, @40 i64 clock_offset_ns [GT], @48 u32 node_id,
  @52 ram_used_kib, @56 ram_total_kib, @60..108 u32 drop and store counters,
  @108..148 f32 rates and errors (pos_error_m @140 is [GT]),
  @148..192 u16 utilisation, queue depths, dcc/cbr, tx power, neighbour and cert counts,
  @192 u8 gnss_fix, @193 u8 node_state (6 = compromised is [GT]), @194 u8 verify_policy,
  @195 u8 reserved8, @196..208 reserved = 0
Unknown at the current tier is 0xFFFF / 0xFFFFFFFF / NaN; every float is quantised at
the writer to its declared grid (ADR 0004 §7): rates, ms, metres and ppm on 1e-3,
gnss_hdop on 1e-4.
";

const METRIC_LAYOUT: &str = "\
vwp.v1.MetricSample body (§3.7): prefix 32 bytes then `sample_count` records of
`record_size` bytes (32 in v1).
prefix: @0 u64 sim_time_ns (bin end), @8 u64 bin_width_ns, @16 u32 sample_count,
  @20 u32 off_samples (8-aligned), @24 u32 record_size, @28 u32 reserved = 0
sample: @0 f64 value (quantised to 1e-6), @8 u32 str_metric, @12 u32 dim_key,
  @16 u32 node_id, @20 u32 count, @24 u16 agg, @26 u8 visibility, @27 u8 reserved,
  @28 u32 prov_id
Units come from the MetricDef, not from the wire. visibility 0 (GT) is not emitted under
the node profile.
";

const EVENT_LAYOUT: &str = "\
vwp.v1.Event body (§3.6): prefix 32 bytes, index block, payload region.
prefix: @0 u64 t_start_ns, @8 u64 t_end_ns, @16 u32 event_count (E),
  @20 u32 off_index (8-aligned), @24 u32 off_payloads (8-aligned), @28 u32 payload_bytes
index block, 16*E bytes, sorted by (sim_time_ns, channel_id):
  u64[E] sim_time_ns, u32[E] payload_off (relative to off_payloads, multiple of 8),
  u16[E] payload_len (padded to 8), u16[E] channel_id (§3.6.2)
Every payload starts 8-aligned and is zero-padded to a multiple of 8; a reader that does
not know a channel_id skips it by payload_len, which is how channels are added without a
version bump (§8.4). Payload layouts are §3.6.4 to §3.6.17.
";

const PROVENANCE_LAYOUT: &str = "\
vwp.v1.Provenance body (§3.8): prefix 32 bytes, entry block, dim block, symbol-table
extension.
prefix: @0 u64 sim_time_ns, @8 u32 entry_count (P), @12 u32 off_entries,
  @16 u32 dim_count (Dk), @20 u32 off_dims, @24 u32 off_strings (0 = none),
  @28 u32 flags (bit0 REPLACE_ALL, bit1 FINAL)
entry block, 24*P bytes, columns: u32 prov_id, u32 str_model_id, u32 str_model_version,
  u32 str_param_set_id, u32 str_card_url, u16 family, u16 subject_kind
dim block, 8*Dk bytes: u32 dim_key, u32 str_dims ('k=v,k=v', keys sorted ASCII-ascending)
";

/// The layout table a frame channel's schema record carries (§7.1).
pub fn layout_for(kind: MsgType) -> String {
    let body = match kind {
        MsgType::Hello => HELLO_LAYOUT,
        MsgType::Keyframe => KEYFRAME_LAYOUT,
        MsgType::Delta => DELTA_LAYOUT,
        MsgType::Telemetry => TELEMETRY_LAYOUT,
        MsgType::Event => EVENT_LAYOUT,
        MsgType::MetricSample => METRIC_LAYOUT,
        MsgType::Provenance => PROVENANCE_LAYOUT,
        MsgType::WorldChunk => {
            "vwp.v1.WorldChunk (§3.9) is not recorded; see the world.vwb attachment.\n"
        }
        MsgType::Error => "vwp.v1.Error (§3.10) is a connection frame and is not recorded.\n",
        MsgType::Bye => "vwp.v1.Bye (§3.11) is a connection frame and is not recorded.\n",
    };
    format!("{HEADER_LAYOUT}\n{body}")
}

/// The layout table a channel carrying a message type this build does not know carries.
///
/// §8.4 makes a new `msg_type` additive and §8.6 has a reader accept a higher minor while
/// ignoring what it does not know, so such a frame is recorded rather than refused
/// (conformance F6, N1). All this build can describe is the header it *can* read, which is
/// exactly the part of the frame that is fixed for the whole of major version 1.
pub fn layout_for_unknown(msg_type: u16) -> String {
    format!(
        "{HEADER_LAYOUT}\nvwp.v1 message type {msg_type:#06x} is not known to this build.\n\
         The 24-byte header above is v1 and was read; the body is stored verbatim and was\n\
         never decoded. §8.4: a new message type id is an additive (minor) change and a\n\
         reader ignores what it does not know; §8.6: the replay reader accepts a higher\n\
         minor. Decode it with a build that implements the minor version in the recording's\n\
         `v2xw.manifest` metadata record.\n"
    )
}

/// The JSON Schema a `record/…` channel's schema record carries.
///
/// Deliberately small: the channel, its visibility, the protocol version and the grid
/// convention. The per-field grids are inferred from the field names by the D9 unit
/// convention ([`crate::export::schema::declared_quantum`]) and are written out in full
/// in the exporter's `schema.json`, where the values they describe actually live.
pub fn record_schema_json(spec: &ChannelSpec) -> String {
    format!(
        "{{\n  \"$schema\": \"https://json-schema.org/draft/2020-12/schema\",\n  \
         \"title\": \"v2xw/{channel}/1\",\n  \"type\": \"object\",\n  \
         \"description\": \"serde Record on channel {channel} (03-interfaces.md §14). \
Every float is quantised at the writer to its field's declared grid (ADR 0004 §7, build \
decision D9); the grid is given per field in the exporter's schema.json.\",\n  \
         \"x-v2xw-visibility\": \"{visibility}\",\n  \
         \"x-v2xw-ground-truth\": {gt},\n  \
         \"x-vwp-channel-id\": {wire_id},\n  \
         \"x-vwp-version\": \"{major}.{minor}\"\n}}\n",
        channel = spec.name,
        visibility = spec.visibility,
        gt = spec.is_gt_tainted(),
        wire_id = match spec.wire_id {
            Some(id) => id.to_string(),
            None => "null".to_string(),
        },
        major = crate::wire::VERSION_MAJOR,
        minor = crate::wire::VERSION_MINOR,
    )
}
