# 03 — Plug-in interfaces, data types, invariants, and schemas

Status: design draft for review (2026-09-17). Companion to `02-architecture.md`.

**Normative source:** the crate `crates/v2xw-core` is the normative source for every signature in this document; this document tracks it, and where the two disagree the crate is right and this document is stale. Last reconciled against the crate on 2026-09-18 (build decision D11).

Every interface here is a plug-in seam; the model families behind each seam are catalogued in `04-models.md`, the protocol seam is developed in `05-protocols.md`, and node internals in `06-node-models.md`.

Conventions used in this document:

- Signatures are given in Rust because the engine core is Rust (ADR 0003). The Python SDK exposes the same interfaces with batched calls (§15). Out-of-process plug-ins speak the same messages over gRPC (§16).
- `Tier` is `Abstract | Medium | High` everywhere. A plug-in declares the tier(s) it implements in its model card; the scenario selects a tier per family. The declaration is mandatory and may not be empty (§12).
- Every trait extends `Model`, which supplies the model card (§12). A plug-in without a model card cannot be registered. The trait itself is in §1.2.
- `&mut dyn Ctx` in the family traits below is shorthand for `&mut dyn Ctx<World = World, Actors = ActorIndex, Payload = Event>`: `Ctx` has three associated types (§1.1), bound once at the call site, and spelling them out in forty signatures would hide what each one says. The same applies to `&dyn NodeView<…>`.
- An **instant** is `SimTime`; a **span** is `Duration` (§1). Signatures below that still spell a span as `SimTime` (`Mobility::step(dt)`, `Phy::air_time`, `ServiceModel::service_time`, `CellularUu::handover`) predate `Duration` and are left unchanged here so that crates already being written against them are not broken; see the open point at the end of §1.2.
- "Invariants" are checked by the conformance test kit (§17). A plug-in that violates one fails registration in CI.
- Units: metres, seconds (as `SimTime` ns ticks internally), radians, dBm/dB, bytes, Hz. Fields carry their unit in the name when ambiguous (`_m`, `_s`, `_ns`, `_dbm`, `_db`, `_hz`, `_bytes`).

## 1. Core types

Everything in this section is implemented in `crates/v2xw-core`; the signatures are that crate's public surface, abbreviated only by dropping bodies and `Debug`/`Display`/serde derives.

```rust
// ---- simulated time (v2xw_core::time) ---------------------------------------------

/// An *instant*: nanoseconds since scenario start t0. u64 gives 584 years of range; the
/// engine guarantees 1 µs resolution to models (ADR 0004 §1).
pub type SimTime = u64;
pub const NS_PER_US: SimTime = 1_000;
pub const NS_PER_MS: SimTime = 1_000_000;
pub const NS_PER_S:  SimTime = 1_000_000_000;

/// A *span* of simulated time: the same machine word as `SimTime`, a different idea.
/// Every delay a model computes (MAC backoff, CAM period, verification service time,
/// DENM repetition interval, protocol timeout) is one of these, and `Ctx::schedule_after`
/// turns it into a deadline, so no model writes `ctx.now() + dt` by hand and gets it
/// wrong. Deliberately not `std::time::Duration`, which measures wall-clock time that
/// nothing in this engine may read. Arithmetic saturates; subtraction clamps at ZERO.
pub struct Duration(pub u64);
impl Duration {
    pub const ZERO: Duration;
    pub const MAX: Duration;                                   // about 584 years
    pub const fn from_nanos(ns: u64) -> Duration;
    pub const fn from_micros(us: u64) -> Duration;
    pub const fn from_millis(ms: u64) -> Duration;
    pub const fn from_secs(s: u64) -> Duration;
    pub fn from_secs_f64(seconds: f64) -> Duration;            // negative and NaN give ZERO
    pub const fn as_nanos(self) -> u64;
    pub fn as_secs_f64(self) -> f64;
    pub const fn is_zero(self) -> bool;
    pub const fn after(self, t: SimTime) -> SimTime;           // "this much after t"
    pub const fn between(from: SimTime, to: SimTime) -> Duration;
    pub const fn saturating_mul(self, k: u64) -> Duration;
    pub const fn checked_div(self, k: u64) -> Option<Duration>;   // None for k == 0
    pub const fn saturating_div(self, k: u64) -> Duration;        // MAX for k == 0
}
// `SimTime + Duration -> SimTime`, `SimTime - Duration -> SimTime`, `Duration ± Duration`
// and `Duration * u64` are defined and saturate.

/// The scenario wall clock: the civil datetime that simulated time zero stands for, and
/// the only wall-clock value in a run besides the manifest's caller-supplied build stamp.
/// It converts a SimTime into the IEEE 1609.2 values certificates, signed messages and
/// CRLs carry. Leap seconds are not modelled.
pub struct WallClock { /* i64 Unix seconds of t0 */ }
impl WallClock {
    pub fn parse_rfc3339(t0: &str) -> Result<WallClock, TimeError>;  // scenario `time.t0`
    pub const fn unix_seconds_at(self, t: SimTime) -> i64;
    pub fn civil_at(self, t: SimTime) -> CivilDateTime;              // proleptic Gregorian UTC
    pub fn time32(self, t: SimTime) -> Result<u32, TimeError>;       // seconds since 2004-01-01Z
    pub fn time64(self, t: SimTime) -> Result<u64, TimeError>;       // microseconds since it
}
pub const IEEE1609_EPOCH_UNIX_S: i64 = 1_072_915_200;
```

### Typed ids

All ids are dense `u32` indices assigned at scenario load and at spawn in a deterministic order (sorted by scenario declaration, then by spawn time, then by spawn sequence). **Ids are never reused within a run.** Every id is `Copy + Ord + Hash + Debug + Display`, serialises transparently as a plain integer, and carries `new(u32)`, `index() -> u32` and `as_usize() -> usize`. `Ord` is load-bearing: parallel phases merge in id order and reductions sort contributors by id before summing (02-architecture §6.4). `Display` prefixes are distinct, so an id in a log names its own type.

```rust
pub struct ActorId(pub u32);      // "a"     vehicle, pedestrian, cyclist (the physical body)
pub struct NodeId(pub u32);       // "n"     OBU, VRU device, RSU, BS, router, backend entity
pub struct LaneId(pub u32);       // "l"
pub struct EdgeId(pub u32);       // "e"     bundle of lanes between two junctions
pub struct JunctionId(pub u32);   // "j"
pub struct SignalId(pub u32);     // "sg"
pub struct BuildingId(pub u32);   // "b"
pub struct CellId(pub u32);       // "c"     cellular cell (a spatial-index cell is `GridCell`)
pub struct SduId(pub u32);        // "sdu"   one service data unit handed down the stack (§5, §6)
pub struct FrameSeq(pub u32);     // "f"     frame counter on one directed link (§4)
pub struct HwProfileId(pub u32);  // "hw"    node hardware profile (§8, 06-node-models §1)
pub struct SiteId(pub u32);       // "site"  RSU or cell mast (`World::sites`, §2)
pub struct CrossingId(pub u32);   // "cr"    pedestrian or cycle crossing (§2)
pub struct LanduseId(pub u32);    // "lu"    land-use zone (§2)
pub struct ConnectionId(pub u32); // "cn"    one movement through a junction (§2)

/// A directed radio link, ordered (tx, rx); `LinkKey(a, b)` and `LinkKey(b, a)` are
/// different keys. Per-link state (fading, shadowing correlation, RNG streams) is keyed
/// by it. Accessors: `tx()`, `rx()`, `reversed()`.
pub struct LinkKey(pub NodeId, pub NodeId);
```

### Geometry, world anchor, ground truth and belief

```rust
/// World-local East-North-Up metres (build decision D6). No f32 in engine state.
pub struct Vec3 { pub x: f64, pub y: f64, pub z: f64 }
// ZERO, new, new_2d, scale, dot, cross, norm, norm_2d, norm_squared, normalized,
// distance, distance_2d, lerp, heading_2d, is_finite; Add, Sub, Mul<f64>, Neg.

pub struct LanePos { pub lane: LaneId, pub s_m: f64, pub d_m: f64 } // longitudinal, lateral
pub struct Dims   { pub length_m: f64, pub width_m: f64, pub height_m: f64 }
pub struct Bbox   { pub min: Vec3, pub max: Vec3 }
// LanePos and Dims each declare `Q_M = 1e-3` and a `quantized()`: they reach an artefact
// inside `Kinematics`, so `Kinematics::quantized` delegates to them.

/// The geodetic anchor of the ENU frame (build decision D6): the world origin, recorded
/// in `WorldProvenance` together with `PROJECTION`.
pub struct GeoOrigin { pub lat_deg: f64, pub lon_deg: f64, pub alt_m: f64 }
impl GeoOrigin {
    pub const PROJECTION: &'static str;    // "equirectangular-local-tangent-plane/1"
    pub const NULL_ISLAND: GeoOrigin;      // a world with no geodetic anchor
    pub const Q_DEG: f64 = 1e-7;           // declared quanta (D9; degrees are declared here)
    pub const Q_ALT_M: f64 = 1e-3;
    pub fn quantized(&self) -> Self;
    pub fn to_enu(&self, lat_deg: f64, lon_deg: f64, alt_m: f64) -> Vec3;
    pub fn to_geodetic(&self, p: Vec3) -> (f64, f64, f64);
    pub fn metres_per_degree_latitude(&self) -> f64;   // WGS-84 series, via crate::math
    pub fn metres_per_degree_longitude(&self) -> f64;
    pub fn is_valid(&self) -> bool;
}

/// Ground-truth kinematic state. Produced by Mobility at each mobility step.
pub struct Kinematics {
    pub t: SimTime,
    pub pos: Vec3,            // reference point = rear-axle centre for vehicles, centroid for VRU
    pub vel: Vec3,            // m/s
    pub acc: Vec3,            // m/s²
    pub heading_rad: f64,     // ENU, 0 = east, counter-clockwise
    pub yaw_rate_rad_s: f64,
    pub lane: Option<LanePos>,
    pub dims: Dims,
}
impl Kinematics {
    pub const Q_M: f64 = 1e-3;          // declared quanta (build decision D9)
    pub const Q_RAD: f64 = 1e-6;
    pub fn quantized(&self) -> Self;    // what the `gt.kinematics` writer emits
    pub fn speed_mps(&self) -> f64;
    pub fn ground_speed_mps(&self) -> f64;
    /// The published constant-velocity extrapolation rule between mobility steps
    /// (ADR 0004 decision 2). It never extrapolates backwards: `t` is clamped to `self.t`.
    pub fn extrapolate(&self, t: SimTime) -> Kinematics;
}

/// A node's *belief* about where and when it is: the GNSS output, never the truth.
/// This is what a generator, a detector, a safety application or an attacker reads.
pub struct PositionEstimate {
    pub pos: Vec3, pub vel: Vec3, pub heading_rad: f64,
    pub semi_major_m: f64, pub semi_minor_m: f64, pub orientation_rad: f64,
    pub time_ns: SimTime, pub fix: FixQuality,
}
impl PositionEstimate {
    pub const Q_M: f64 = 1e-3;          // declared quanta (build decision D9)
    pub const Q_RAD: f64 = 1e-6;
    pub fn no_fix(time_ns: SimTime) -> Self;
    pub fn perfect(k: &Kinematics) -> Self;          // for tests and the "ideal GNSS" model
    pub fn ground_speed_mps(&self) -> f64;
    pub fn error_ellipse_area_m2(&self) -> f64;
    pub fn uncertainty_towards_m(&self, bearing_rad: f64) -> f64;
    pub fn is_well_formed(&self) -> bool;            // finite, semi_major >= semi_minor >= 0
    pub fn quantized(&self) -> Self;                 // what a recorder or exporter writes
}
pub enum FixQuality { NoFix, DeadReckoning, TwoD, ThreeD, Differential, Rtk }
// has_position(), is_three_d(), is_satellite_fix(); serde spelling is kebab-case.

/// Weather at a point in space and time, and what it does to driving (§3, §4).
pub struct WeatherState {
    pub kind: WeatherKind,        // Clear | Rain | Snow | Sleet | Fog | Wind
    pub intensity: f64,           // 0..=1, clamped by `new`
    pub visibility_m: f64,        // INFINITY when unrestricted
    pub surface: SurfaceCondition, // Dry | Wet | Flooded | Snow | Ice
}
impl WeatherState {
    pub const CLEAR: WeatherState;
    pub const Q_VISIBILITY_M: f64 = 1e-3;
    pub const Q_INTENSITY: f64 = 1e-4;
    pub fn new(kind: WeatherKind, intensity: f64, visibility_m: f64, surface: SurfaceCondition) -> Self;
    pub fn is_visibility_reduced(&self) -> bool;
    pub fn is_well_formed(&self) -> bool;
    pub fn quantized(&self) -> Self;
}
pub struct DrivingEffects {
    pub desired_speed_factor: f64, pub headway_factor: f64,
    pub max_decel_mps2: f64, pub visibility_m: f64,
}
impl DrivingEffects {
    pub const UNAFFECTED: DrivingEffects;
    pub const Q_FACTOR: f64 = 1e-4;
    pub const Q_M: f64 = 1e-3;
    pub fn apply_to_desired_speed(&self, v_mps: f64) -> f64;
    pub fn apply_to_headway(&self, t_s: f64) -> f64;
    pub fn cap_decel(&self, a_mps2: f64) -> f64;
    pub fn is_well_formed(&self) -> bool;
    pub fn quantized(&self) -> Self;
}
```

The three fields that carry a non-finite in-band sentinel — `WeatherState::visibility_m`, `DrivingEffects::{max_decel_mps2, visibility_m}` and `PositionEstimate::{semi_major_m, semi_minor_m}` — are encoded with `#[serde(with = "v2xw_core::serde_sentinel::f64_inf")]`: the sentinel is written as JSON `null` (which is what `serde_json` wrote for it anyway) and `null` is read back as `f64::INFINITY`, so `WeatherState::CLEAR`, `DrivingEffects::UNAFFECTED` and `PositionEstimate::no_fix` survive their own round trip. Without it a JSON or JSONL exporter could write a clear-weather keyframe and then fail to read it back.

Every type that can reach a recorded, exported or digested artefact carries a `quantized()` and declares its quanta as `Q_*` constants. That is build decision D9 and ADR 0004 decision 7 in code: no float reaches an artefact in raw IEEE-754 form. The general spellings are `math::quantize_to(x, quantum)`, `math::quantize(x, decimals)`, `math::q3(x)` (the legacy 3-decimal convention for metres and seconds) and the predicate `math::is_on_grid(x, quantum)` that the output-scanning test uses. A **digest** hashes `math::grid_index(x, quantum) -> i64`, the integer multiple of the quantum, rather than the rounded float: the quantised `f64` is still a binary approximation of a decimal grid point, and the integer is what two platforms are guaranteed to agree on.

Transcendentals never come from the platform libm. `v2xw_core::math` re-exports the pure-Rust `libm` crate (`sin`, `cos`, `sin_cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `exp2`, `ln`, `log10`, `log2`, `pow`, `cbrt`, `hypot`, `sinh`, `cosh`, `tanh`), and `math::sqrt` is the one IEEE-exact exception (ADR 0003, ADR 0004 §4). Ordered reductions are `math::sum_ordered` and `math::sum_sorted_by_key`. `f64` has no `Ord`, so any order over floats — a median, a p95, a sorted dump — goes through `math::sort_total_order(&mut [f64])` (IEEE total order, via `f64::total_cmp`) and `math::quantile_sorted(&[f64], q)`, whose interpolation rule is fixed and documented (Hyndman & Fan type 7, `h = (n − 1)·q`), so two engines cannot disagree about a recorded p95.

### Tiers and the event order

```rust
pub enum Tier { Abstract, Medium, High }    // serde: "abstract" | "medium" | "high"

/// Fixed priorities per class (02-architecture §5.1); plug-ins never choose a priority.
pub enum EventClass {
    Control, MobilityStep, SignalPhase, PhyEnd, MacTimer,
    PhyStart, NodeTask, NetDeliver, FlowTimer, Observe,
}

/// Total order of events: (time, priority, seq). `seq` is assigned by the scheduler at
/// schedule time from a single monotonic counter, so two events at the same time and
/// priority run in the order they were scheduled.
pub struct EventKey { pub time: SimTime, pub priority: u8, pub seq: u64 }

/// What `Ctx::schedule` returns and `Ctx::cancel` consumes: the scheduled event's `seq`.
/// Cancellation is lazy: the entry stays in the heap and is skipped when it surfaces.
pub struct EventHandle(pub u64);

/// The kernel's heap, generic over the engine's own event payload enum (build decision D8:
/// the concrete `Event` enum lives in `v2xw-engine`, not in the contract crate).
pub struct Scheduler<E> { /* … */ }
impl<E> Scheduler<E> {
    pub fn schedule(&mut self, at: SimTime, class: EventClass, payload: E) -> EventHandle;
    pub fn schedule_after(&mut self, delay: Duration, class: EventClass, payload: E) -> EventHandle;
    pub fn schedule_reentrant(&mut self, at: SimTime, class: EventClass, payload: E) -> EventHandle;
    pub fn cancel(&mut self, h: EventHandle) -> bool;
    pub fn pop(&mut self) -> Option<(EventKey, E)>;
    pub fn peek_key(&mut self) -> Option<EventKey>;
    pub fn now(&self) -> SimTime;
}
```

### 1.1 Engine context handed to every plug-in call

`Ctx` is the only thing a model sees of the engine: the clock, its RNG streams, the event heap, the world and the actor index, the recorder, the provenance log and its own resolved parameters. Every family trait in this document is declared against it.

```rust
pub trait Ctx {
    /// The world model, `v2xw_world::World` in a real engine.
    type World;
    /// The spatial index over the current actor kinematics, `v2xw_world::ActorIndex`.
    type Actors;
    /// The kernel's event payload enum, which instantiates `Scheduler<E>`.
    type Payload;

    fn now(&self) -> SimTime;

    /// The deterministic stream for (domain, entity) (02-architecture §6.2). A plug-in
    /// never owns an RNG; it asks for the stream of the entity it is acting for. Takes
    /// `&self` and hands back a guard, for the two reasons below.
    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle;

    /// "Now plus this much", which is what a timer, a backoff, a service time or a
    /// repetition interval is. Defaulted in terms of `schedule` and saturating, so no
    /// model computes the deadline itself.
    fn schedule_after(&mut self, delay: Duration, class: EventClass, payload: Self::Payload)
        -> EventHandle { self.schedule(delay.after(self.now()), class, payload) }

    fn cancel(&mut self, handle: EventHandle) -> bool;
    fn world(&self) -> &Self::World;
    fn actors(&self) -> &Self::Actors;

    /// The object-safe recorder primitive an engine implements. Callers use `CtxExt::emit`.
    fn emit_erased(&mut self, record: &dyn ErasedRecord);

    /// Provenance: attach (model, parameters) to a value the UI or an exporter may show.
    /// Cheap enough for a hot path: three handles, deduplicated by the whole triple.
    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId);

    /// This instance's resolved parameters: card defaults merged with scenario overrides.
    /// Every number a model reads comes from here and is declared on its card (I-C3).
    fn params(&self) -> &ParamSet;
}

/// The generic conveniences on top of the dyn-compatible `Ctx`, blanket-implemented for
/// every `Ctx` including `dyn Ctx`, so a plug-in holding `&mut dyn Ctx<…>` writes
/// `ctx.emit(record)`.
pub trait CtxExt: Ctx {
    fn emit<R: Record>(&mut self, record: R) { self.emit_erased(&record); }
}
impl<C: Ctx + ?Sized> CtxExt for C {}
```

The recorder seam (§14) is two traits: the typed one a plug-in writes, and the erased one the engine consumes.

```rust
/// One Rust type per recording channel, carrying the channel's stable id and its
/// visibility tag. `Serialize` is the lowest common denominator every backend can
/// consume; a recorder that knows the channel downcasts through `as_any` and takes a
/// faster path (Arrow batches, Parquet columns) instead of paying for JSON.
pub trait Record: Serialize + 'static {
    const CHANNEL: &'static str;          // "node.tx", "metric.sample", …
    const VISIBILITY: Visibility;
    /// Defaults to `VISIBILITY`; override only where the tag genuinely varies per
    /// instance (a `phy.rx` written without the transmitter's identity is `Node`, with
    /// it `NodeAndGt`).
    fn visibility(&self) -> Visibility { Self::VISIBILITY }
}

/// A `Record` seen through a trait object. Blanket-implemented for every `Record`;
/// engines consume it, plug-ins never name it.
pub trait ErasedRecord {
    fn channel(&self) -> &'static str;
    fn visibility(&self) -> Visibility;
    fn write_json(&self, out: &mut Vec<u8>) -> Result<()>;
    fn as_any(&self) -> &dyn core::any::Any;
    /// The owned form (§10's `EventRecord`), for a recorder that cannot keep the
    /// call-scoped borrow: one that batches, shards by channel or queues to a writer
    /// thread. Defaulted in terms of the three methods above.
    fn to_owned_record(&self) -> Result<OwnedRecord> { /* … */ }
}
impl<R: Record> ErasedRecord for R { /* … */ }

/// Who may see a recorded value (§14). The tag travels with the record rather than with
/// the channel, so one record type can be honest about carrying both.
pub enum Visibility { Gt, Node, NodeAndGt, Public, Derived, Mixed, Meta }
impl Visibility {
    pub const fn is_gt_tainted(self) -> bool;          // Gt | NodeAndGt | Mixed
    pub const fn allowed_on_node_channel(self) -> bool; // Node | Public
}
```

The RNG seam. Keys are `SHA-256(master_seed ‖ domain ‖ entity)`, so a draw depends only on who is drawing and what for, never on event order or thread count (ADR 0004 §3).

```rust
/// What a draw is *for*. One domain names one purpose: two models drawing from the same
/// (domain, entity) key interleave, and each then depends on the order the other ran in.
/// Each variant has a fixed numeric code (built-ins 1..=18) that is part of the
/// determinism contract; changing a code changes every golden digest.
pub enum RngDomain {
    Spawn, Mobility, LaneChange, Gnss, Shadow, Fading, MacBackoff, SpsSelection,
    AbstractRx, Attack, ServiceTime, Backend, Collusion, Perception, Report, Crypto,
    DesiredSpeed, ReactionTime,
    /// An out-of-tree plug-in's own domain, derived from its model id rather than
    /// hand-picked: `0x8000_0000 | (LE_u32(SHA-256(model_id)[0..4]) >> 1)`. The high bit
    /// is always set, so a plug-in code can never collide with a built-in one.
    Plugin(PluginDomain),
}
impl RngDomain { pub fn plugin(model_id: &str) -> RngDomain; pub const fn code(&self) -> u32; }

/// Who is drawing. The encoding is explicit (a one-byte tag plus little-endian fields)
/// rather than derived from Rust's `Hash`, whose output is not specified across versions.
pub enum EntityRef {
    Global,
    Actor(ActorId),
    Node(NodeId),
    Link(LinkKey),
    /// A single frame on a directed link: the scope of a small-scale fading draw.
    /// **Single-use**: the key already embeds the counter, so the stream is never cached.
    LinkFrame { link: LinkKey, frame: u64 },
    Lane(LaneId),
    Signal(SignalId),
    /// A plug-in's own scope; build it with `EntityRef::custom(model_id, id)`, which
    /// derives `kind` from the model id instead of letting two authors both pick 1.
    Custom { kind: u16, id: u64 },
}
impl EntityRef {
    pub fn custom(model_id: &str, id: u64) -> EntityRef;
    pub const fn is_single_use(&self) -> bool;   // true only for LinkFrame, today
    pub fn encode(&self, out: &mut Vec<u8>);
}

/// One deterministic stream: a ChaCha12 keystream plus this crate's samplers. Every
/// sampler has a documented algorithm and a **fixed draw count**, so a model's position
/// in its stream never depends on the values it drew.
pub struct RngStream { /* … */ }
impl RngStream {
    pub fn u64(&mut self) -> u64;
    pub fn u32(&mut self) -> u32;                       // one 32-bit word, not half a u64
    pub fn fill_bytes(&mut self, dest: &mut [u8]);
    pub fn f64(&mut self) -> f64;                       // (u64 >> 11) × 2⁻⁵³, exact
    pub fn bool(&mut self, p: f64) -> bool;             // one draw whatever p is
    pub fn uniform(&mut self, a: f64, b: f64) -> f64;
    pub fn below(&mut self, n: u64) -> u64;             // Lemire, no rejection loop
    pub fn normal(&mut self, mu: f64, sigma: f64) -> f64;        // Box-Muller, fixed branch, 2 draws
    pub fn exponential(&mut self, rate: f64) -> f64;             // inversion, 1 draw
    pub fn lognormal(&mut self, mu: f64, sigma: f64) -> f64;     // exp(normal), 2 draws
    pub fn gamma(&mut self, shape: f64, scale: f64) -> f64;      // Marsaglia-Tsang
    pub fn nakagami(&mut self, m: f64, omega: f64) -> f64;       // amplitude, sqrt(gamma)
    pub fn choose_index(&mut self, weights: &[f64]) -> usize;    // caller supplies a fixed order
    pub fn word_pos(&self) -> u128;                     // snapshot / replay
    pub fn set_word_pos(&mut self, pos: u128);
}

/// A stream borrowed out of the registry and returned to it on drop. Derefs to
/// `RngStream`, so `ctx.rng(d, e).normal(0.0, sigma)` reads like a method call.
pub struct RngGuard<'a> { /* … */ }

/// The set of live streams for a run. `checkout` takes `&self` and is the one sanctioned
/// way to draw; the cache is sharded and individually locked, so it works inside the
/// phase-parallel maps of 02-architecture §6.4.
pub struct RngRegistry { /* … */ }
impl RngRegistry {
    pub fn new(master_seed: u64) -> RngRegistry;
    pub fn checkout(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_>;
    pub fn stream(&mut self, domain: RngDomain, entity: EntityRef) -> &mut RngStream; // event loop only
    pub fn ephemeral(&self, domain: RngDomain, entity: EntityRef) -> RngStream;       // uncached, word 0
    pub fn forget(&mut self, domain: RngDomain, entity: EntityRef) -> bool;           // despawn path
}
```

Two properties of that seam are contract, not implementation detail:

- **Per-link streams do not accumulate.** A fading draw is scoped to `(link, frame)`, whose key already embeds the frame counter. Such a scope is `is_single_use`, so `checkout` derives it, uses it and drops it rather than interning a generator per frame per directed link that nothing would ever free. The cache is therefore bounded by the number of live entities, and `forget` reclaims a despawned entity's streams (safe because ids are never reused within a run).
- **Two tasks may not share one key.** `checkout` panics if the same key is already checked out, by another thread or by a guard this thread still holds. Interleaving two draw sequences in thread-scheduling order is exactly the nondeterminism the module exists to prevent, so it fails loudly instead of silently re-deriving.

#### Why `Ctx` is shaped this way

This subsection exists because the obvious shape is wrong in three ways that only show up once real model code is written, and each "fix" back to the obvious shape is tempting.

**1. `rng` takes `&self` and returns a guard, not `&mut self` returning `&mut RngStream`.** With a `&mut self` accessor, this compiles:

```rust
let noise  = ctx.rng(Mobility, actor).normal(0.0, sigma);  // borrow ends here
let leader = ctx.world().leader_of(actor);                 // fine
```

and this does not:

```rust
let d      = ctx.world().lane(l).length_m;                 // &self borrow, live below
let jitter = ctx.rng(Mobility, actor).f64() * d;           // &mut self: will not compile
```

The second shape is the first thing real model code does: read something from the world, then perturb it. A `&mut self` accessor also makes the whole context unusable inside the phase-parallel maps of 02-architecture §6.4, where it is shared by reference across tasks. With `&self` both compile, and the borrow checker still refuses to let a guard be held across a `&mut self` call, which is what we want: that call may schedule an event, and the stream must be back in the registry before the next task asks for it. The cost is that the registry caches behind shard locks, which is why it can hand out a `&self` borrow at all.

**2. `emit` is split into `emit_erased` plus a blanket-implemented `CtxExt::emit`.** In-process Rust plug-ins are trait objects (ADR 0007 §8): a family trait method takes `&mut dyn Ctx<World = …, Actors = …, Payload = …>`. A generic method `fn emit<R: Record>(&mut self, r: R)` on `Ctx` itself makes the trait non-dyn-compatible, so *no* family trait could be used as a trait object and the whole plug-in system would have to become generic. Splitting it keeps one object-safe primitive for engines and the published generic ergonomics for callers, because `CtxExt` is blanket-implemented for `C: Ctx + ?Sized`, which includes `dyn Ctx`. The one thing a caller must remember is to bring `CtxExt` into scope (`use v2xw_core::CtxExt;`); a model that gets "no method named `emit`" is missing that import, not a method.

**3. `World`, `Actors` and `Payload` are associated types.** The contract crate holds no domain models (ADR 0010): the world and the actor index live in `v2xw-world` and the concrete event enum in `v2xw-engine` (build decision D8), both of which depend on the contract crate rather than the other way round. Naming them concretely here would invert the dependency. A call site writes them out: `&mut dyn Ctx<World = World, Actors = ActorIndex, Payload = Event>`.

A plug-in body written against the published shape. The crate compiles the same shape as a test (`ctx.rs`, `a_plugin_can_be_written_against_a_dyn_ctx`), with stand-in associated types, so the ergonomics published here cannot silently stop holding:

```rust
fn mobility_step(ctx: &mut dyn Ctx<World = World, Actors = ActorIndex, Payload = Event>,
                 model: ModelRef, params: ParamSetId) {
    let t = ctx.now();
    for actor in ctx.actors().iter() {
        let lane_len = ctx.world().lane_of(actor).length_m;        // &self borrow, held
        let sigma    = ctx.params().get_f64("sigma_m").unwrap_or(1.0);
        let noise    = ctx.rng(RngDomain::Mobility, EntityRef::Actor(actor)).normal(0.0, sigma);
        let x_m      = math::q3(lane_len + noise);                 // quantise at the writer
        ctx.emit(GtKinematics { t, actor, x_m });                  // CtxExt; &mut self, guard dropped
        ctx.why(ProvSubject::actor(actor, "x_m"), model, params);
    }
    ctx.schedule_after(Duration::from_millis(100), EventClass::MobilityStep, Event::MobStep);
}
```

### 1.2 The `Model` base trait, the registry, and parameters

The conventions above say every family trait extends `Model`. This is that trait. It is dyn-compatible for the same reason `Ctx` is: no generic methods, no `Self`-typed arguments or returns, no associated constants.

```rust
pub trait Model {
    /// This model's card (§12). The only method an implementor writes; it must return the
    /// card that was registered, because the registry hashes it for the content hash the
    /// manifest pins. In practice it is a field, built once.
    fn card(&self) -> &ModelCard;

    fn tiers(&self) -> &[Tier] { &self.card().tier }             // never empty: §12
    fn implements_tier(&self, tier: Tier) -> bool { self.card().implements_tier(tier) }
    fn id(&self) -> &str { &self.card().id }             // "radio/propagation/log-distance-shadowing"
    fn version(&self) -> &str { &self.card().version }
    fn family(&self) -> Family { self.card().family }
    fn id_at_version(&self) -> String { self.card().id_at_version() }   // "id@version"
}

/// How the registry stores a live model. `Send + Sync` because the registry is read from
/// inside the phase-parallel maps; the bound also refuses a model that captured an `Rc`,
/// a `Cell` or a thread-local, which are the shapes that make behaviour depend on where
/// the code ran. A family trait object (`Arc<dyn Propagation>`) is *not* this type and
/// cannot be coerced to it: Rust has no `Arc` trait-object upcasting across a supertrait.
/// A crate that needs both keeps both handles to the same `Arc`.
pub type ModelHandle = std::sync::Arc<dyn Model + Send + Sync>;

/// Dense handles to a registration and to an interned parameter set. These two, plus a
/// `ProvSubject`, are all `Ctx::why` stores, which is why provenance is cheap enough to
/// call unconditionally on a hot path. Both display as `m7` / `p3` and serialise as bare
/// integers; both carry `new(u32)` and `index()`.
pub struct ModelRef(pub u32);
pub struct ParamSetId(pub u32);

pub struct Registry { /* … */ }
impl Registry {
    pub fn register(&mut self, card: ModelCard) -> Result<ModelRef, RegistryError>;
    pub fn register_with_licence(&mut self, card: ModelCard, licence: Licence) -> Result<ModelRef, RegistryError>;
    pub fn register_out_of_process(&mut self, card: ModelCard, licence: Licence) -> Result<ModelRef, RegistryError>;
    /// Registers a live model: its card *and* the object. The card comes from
    /// `Model::card`, so metadata and object cannot disagree.
    pub fn register_model(&mut self, model: ModelHandle) -> Result<ModelRef, RegistryError>;
    pub fn register_model_with_licence(&mut self, model: ModelHandle, licence: Licence) -> Result<ModelRef, RegistryError>;
    pub fn register_model_out_of_process(&mut self, model: ModelHandle, licence: Licence) -> Result<ModelRef, RegistryError>;
    pub fn get_model(&self, r: ModelRef) -> Option<&ModelHandle>;
    pub fn resolve(&self, id: &str) -> Option<ModelRef>;
    pub fn iter_by_id(&self) -> impl Iterator<Item = (ModelRef, &RegisteredModel)>;
    pub fn todo_calibrate_report(&self) -> Vec<(ModelRef, Parameter)>;   // registry rule R1
    pub fn model_card_versions(&self) -> Vec<(String, String)>;          // for the manifest
}
pub struct RegisteredModel {
    pub card: ModelCard, pub licence: Licence, pub hosting: Hosting, pub content_hash: [u8; 32],
}
pub enum Hosting { InProcess, OutOfProcess }
pub enum Licence { Mit, Bsd, Gpl, Lgpl, Other(String), Unspecified }  // ADR 0007 §5 gate

/// A plug-in instance's resolved parameters, name-ordered, content-addressed.
pub struct ParamSet { /* … */ }
impl ParamSet {
    /// Card defaults merged with the scenario's overrides, with four checks: the
    /// overrides must be a JSON object; every overridden name must be declared by the
    /// card (a scenario typo is an error, not a silent fallback to the default); the
    /// override's JSON type must match the declared default's; and the card's `range`
    /// must hold, read as `[min, max]` inclusive exactly when it has two numeric entries
    /// and the value is a number, otherwise as the set of allowed values.
    pub fn resolve(defaults: &ModelCard, overrides: &serde_json::Value) -> Result<ParamSet>;
    pub fn get(&self, name: &str) -> Option<&serde_json::Value>;
    pub fn get_f64(&self, name: &str) -> Option<f64>;
    pub fn get_u64(&self, name: &str) -> Option<u64>;
    pub fn get_bool(&self, name: &str) -> Option<bool>;
    pub fn get_str(&self, name: &str) -> Option<&str>;
    pub fn content_hash(&self) -> [u8; 32];            // over canonical JSON
}
pub struct ParamSetStore { /* interns ParamSets to ParamSetIds by content hash */ }
```

`NodeView` is the belief-only counterpart of `Ctx`, and it is invariant I-C2 in code rather than in prose. It is a trait, not a struct, because the types a node's view is *of* live in the crates above the contract crate:

```rust
pub trait NodeView {
    type Neighbors;    // v2xw-node's neighbour table
    type Credential;   // v2xw-sec's credential handle
    type Message;      // v2xw-msg's verified message

    fn node(&self) -> NodeId;
    fn believed_time(&self) -> SimTime;          // the node's drifting clock, not `Ctx::now`
    fn position(&self) -> &PositionEstimate;     // the node's belief, not `Kinematics`
    fn fix(&self) -> FixQuality { self.position().fix }
    fn neighbors(&self) -> &Self::Neighbors;
    fn credentials(&self) -> &[Self::Credential];
    fn active_credential(&self) -> Option<&Self::Credential> { self.credentials().first() }
    fn received(&self) -> &[Self::Message];
    fn last_received(&self) -> Option<&Self::Message> { self.received().last() }
}
```

It has no `world()`, no `actors()`, no `Kinematics` and no `ActorId` anywhere in its surface. A family-trait method spells it as a trait object, which is why signatures below read `&dyn NodeView<Neighbors = …, Credential = …, Message = …>` rather than the `&NodeView` of earlier drafts (a bare trait name is not a type).

Invariants (all plug-ins):

- I-C1 A plug-in may not keep wall-clock time, thread ids, or process-global RNG state. Conformance kit runs the plug-in twice and compares emitted records byte for byte.
- I-C2 A plug-in may not read `World` ground truth for a node's *belief* unless the interface explicitly passes it (e.g., `Mobility` gets GT; `Detector` does not). The Python SDK enforces this by handing `Detector` a `NodeView`, not the world.
- I-C3 Every numeric parameter a plug-in reads must be declared in its model card with unit, default, and source, or the registry rejects it (§12).

Open points for arbitration (recorded 2026-09-18, not yet decided):

- **A node-side context.** I-C2 and I-T1 are structural only for a method whose arguments cannot reach the truth. `Ctx` has `world()` and `actors()`, so a node-side signature that passes **both** a `Ctx` and a `NodeView` (as §6's `MessageGenerator` and §9's `Detector` do, and as §9's `Attacker` does alongside its `AttackerView`) leaves the invariant resting on review rather than on the compiler. Resolving it means giving the node-side families a context without `world()`/`actors()`; the crate does not define one today.
- **Spans still spelled `SimTime`.** `Duration` exists precisely so that a delay cannot be confused with an instant, but the family traits below predate it and still take or return `SimTime` where they mean a span (`Mobility::step(dt)`, `Phy::air_time`, `ServiceModel::service_time`, `CellularUu::handover`). Changing them is a one-line-per-signature edit with no behavioural effect, but it is a breaking change for crates being written now, so it is recorded rather than applied.
- **A family's selected tier.** Several family traits declare `fn tier(&self) -> Tier` while `Model::tiers()` already answers, from the card, which tiers the model implements (and a model may implement several). Whether `tier()` should mean "the tier the scenario selected for this instance" or be dropped in favour of `Model::implements_tier` needs one decision, applied to every family at once.

## 2. World

```rust
pub trait WorldSource: Model {
    /// Build a World from a source description; must be pure given (source, options, cache).
    fn build(&self, src: &WorldSourceSpec, opts: &ImportOptions) -> Result<World, WorldError>;
}

pub struct World {
    pub origin: GeoOrigin,                // lat, lon, alt, projection (local tangent plane)
    pub bbox: Bbox,
    pub roads: RoadNetwork,
    pub buildings: Vec<Building>,         // footprint polygon, height_m, material class, LOD hints
    pub terrain: Option<Terrain>,         // DEM raster (heights on a grid) + interpolation rule
    pub signals: Vec<SignalPlan>,
    pub sites: Vec<Site>,                 // RSU / cell sites with antenna height
    pub landuse: Vec<LanduseZone>,        // for propagation environment presets and rendering
    pub provenance: WorldProvenance,      // source, bbox, import date, transformations, licence
    pub content_hash: [u8; 32],
}

pub struct RoadNetwork { /* lane-level graph, 02-architecture §4 and 04-models §1 */ }
impl RoadNetwork {
    pub fn lane(&self, id: LaneId) -> &Lane;                       // centreline polyline with z, width, speed limit, allowed classes, type (drive/bike/sidewalk/bus)
    pub fn successors(&self, lane: LaneId) -> &[Connection];        // via junction internal lanes
    pub fn junction(&self, id: JunctionId) -> &Junction;            // internal lanes, conflict matrix, control (signal/priority/stop/yield/roundabout)
    pub fn crossings(&self) -> &[Crossing];
    pub fn project(&self, p: Vec3) -> Option<LanePos>;              // nearest lane, deterministic tie-break by LaneId
    pub fn to_xyz(&self, lp: &LanePos) -> Vec3;
}

pub trait ObstacleModel: Model {
    /// Line-of-sight classification and obstruction geometry between two points using the
    /// world's buildings, terrain and (optionally) current actors as obstacles.
    fn los(&self, world: &World, a: Vec3, b: Vec3, actors: Option<&ActorIndex>) -> LosResult;
}
pub struct LosResult {
    pub class: LosClass,                 // Los | NlosB (building) | NlosV (vehicle) | NlosT (terrain) | NlosBV
    pub walls_crossed: u16,              // building outline intersections (Sommer 2011 model input)
    pub obstructed_len_m: f64,           // length of segment inside buildings
    pub knife_edges: SmallVec<[KnifeEdge; 4]>, // for vehicle/terrain diffraction models
}
```

Invariants: I-W1 `World` is immutable after load except for `SignalPlan` state and dynamic closures applied through events; I-W2 `content_hash` covers all geometry and is written to the manifest; I-W3 importers record every transformation (projection, simplification tolerance, height defaulting rule) in `provenance`.

## 3. Mobility and actors

```rust
pub trait Mobility: Model {
    fn tier(&self) -> Tier;
    fn init(&mut self, ctx: &mut dyn Ctx, world: &World, demand: &dyn Demand) -> Result<(), MobError>;
    /// Advance all actors by dt. Must be a pure function of (state, dt, RNG streams).
    /// Returns kinematics for every active actor plus spawn/despawn lists.
    fn step(&mut self, ctx: &mut dyn Ctx, dt: SimTime) -> MobilityUpdate;
    /// Apply an external control (route change, speed cap, stop, park, closure) at the next step.
    fn command(&mut self, ctx: &mut dyn Ctx, cmd: MobilityCommand);
    /// Optional GT queries for perception and safety-outcome metrics.
    fn kinematics(&self, a: ActorId) -> Option<&Kinematics>;
}
pub struct MobilityUpdate { pub t: SimTime, pub states: Vec<(ActorId, Kinematics)>, pub spawned: Vec<ActorSpawn>, pub despawned: Vec<(ActorId, DespawnCause)>, pub signal_states: Vec<(SignalId, PhaseState)> }

pub trait CarFollowing: Model {  // used by the native medium tier; SUMO tier ignores it
    fn accel(&self, ego: &VehicleView, leader: Option<&LeaderView>, lane: &LaneView, w: &WeatherState) -> f64;
}
pub trait LaneChange: Model { fn decide(&self, ctx: &mut dyn Ctx, ego: &VehicleView, nbrs: &LaneNeighbors, w: &WeatherState) -> LaneChangeDecision; }
pub trait IntersectionControl: Model { fn may_enter(&self, ego: &VehicleView, j: &JunctionView, conflicts: &[ConflictView], w: &WeatherState) -> EntryDecision; }
pub trait Router: Model { fn route(&self, ctx: &mut dyn Ctx, from: LaneId, to: LaneId, at: SimTime, costs: &dyn EdgeCost) -> Option<Route>; fn reroute_policy(&self) -> ReroutePolicy; }
pub trait Demand: Model { fn spawns_in(&mut self, ctx: &mut dyn Ctx, from: SimTime, to: SimTime) -> Vec<TripRequest>; }
pub trait VruMobility: Model { fn step(&mut self, ctx: &mut dyn Ctx, dt: SimTime, world: &World, vehicles: &ActorIndex) -> Vec<(ActorId, Kinematics)>; }

pub trait WeatherModel: Model {
    fn state_at(&self, t: SimTime, p: Vec3) -> WeatherState;  // type, intensity, visibility_m, surface
    fn driving_effects(&self, w: &WeatherState) -> DrivingEffects; // desired-speed factor, headway factor, decel cap, visibility (04-models §2.6)
}
pub trait GnssModel: Model {
    /// Node belief of its own position/time given GT; stateful per node (Gauss–Markov error, outages, jamming).
    fn estimate(&mut self, ctx: &mut dyn Ctx, node: NodeId, gt: &Kinematics, env: &GnssEnv) -> PositionEstimate;
}
// `PositionEstimate`, `FixQuality`, `WeatherState` and `DrivingEffects` are core types, defined once in §1.
pub trait ClockModel: Model { fn read(&mut self, ctx: &mut dyn Ctx, node: NodeId, gnss: &FixQuality) -> SimTime; /* node-believed time incl. drift */ }
```

Invariants: I-M1 `step` output ordering is by `ActorId`; I-M2 spawn ids are assigned from the demand stream order, independent of thread count; I-M3 the SUMO tier must report its version and RNG seed in the manifest and must be driven with a fixed step equal to the engine's mobility step; I-M4 all tiers publish `Kinematics` in the same frame and reference point.

## 4. Radio

```rust
pub struct RadioEndpoint { pub node: NodeId, pub pos: Vec3 /* antenna phase centre */, pub gain_dbi: f64, pub pattern: Option<PatternRef>, pub pos_time: SimTime, pub class: ActorClass }

pub trait Propagation: Model {
    fn tier(&self) -> Tier;
    /// Large-scale loss (path loss + obstacle shadowing + weather + antenna pattern), dB, deterministic.
    fn loss_db(&self, ctx: &mut dyn Ctx, tx: &RadioEndpoint, rx: &RadioEndpoint, f_hz: f64, los: &LosResult, w: &WeatherState) -> LossBreakdown;
    /// Environment preset id used for parameters (urban/suburban/highway/rural), from world land use.
    fn environment(&self, world: &World, p: Vec3) -> EnvClass;
}
pub struct LossBreakdown { pub path_db: f64, pub shadow_db: f64, pub obstacle_db: f64, pub weather_db: f64, pub antenna_db: f64, pub total_db: f64 }

pub trait Fading: Model { fn sample_db(&mut self, ctx: &mut dyn Ctx, link: LinkKey, d_m: f64, t: SimTime) -> f64; }

pub struct FrameDescriptor { pub bytes: u32, pub mcs: Mcs, pub tx_power_dbm: f64, pub channel: ChannelId, pub ac: AccessCategory, pub kind: FrameKind, pub sdu_ref: SduRef }

pub trait Phy: Model {
    fn tier(&self) -> Tier;
    fn rat(&self) -> Rat; // Dsrc80211p | LteV2xPc5 | NrV2xPc5
    /// Begin a transmission: computes air time from (bytes, mcs), schedules `end_tx`, and
    /// (high/medium tiers) registers the frame at every receiver in range as an `Arrival`.
    fn begin_tx(&mut self, ctx: &mut dyn Ctx, tx: NodeId, f: &FrameDescriptor) -> TxHandle;
    fn air_time(&self, bytes: u32, mcs: Mcs) -> SimTime;  // exact per 802.11-2016 17.4.3 / 3GPP TBS rules
    fn cca(&self, ctx: &dyn Ctx, node: NodeId, ch: ChannelId) -> CcaState; // Idle | Busy{energy_dbm}
    /// Called at the scheduled end of each arrival; returns outcome and loss cause.
    fn finish_rx(&mut self, ctx: &mut dyn Ctx, rx: NodeId, h: RxHandle) -> RxOutcome;
    fn noise_floor_dbm(&self, node: NodeId, ch: ChannelId) -> f64;
}
pub enum RxOutcome { Received { sinr_db: f64, rssi_dbm: f64 }, Lost(LossCause) }
pub enum LossCause { OutOfRange, BelowSensitivity, Collision, PreambleMissed, HalfDuplex, HiddenTerminal, Jammed, Fading, InBandEmission, ResourceCollision, Abstract /* abstract tier draw */ }

pub trait Mac: Model {
    fn tier(&self) -> Tier;
    fn enqueue(&mut self, ctx: &mut dyn Ctx, node: NodeId, sdu: MacSdu, ac: AccessCategory) -> Result<(), DropCause>;
    fn on_cca(&mut self, ctx: &mut dyn Ctx, node: NodeId, ch: ChannelId, state: CcaState);
    fn on_tx_done(&mut self, ctx: &mut dyn Ctx, node: NodeId, h: TxHandle);
    fn cbr(&self, node: NodeId, ch: ChannelId) -> f64; // last measurement window
    fn resource_model(&self) -> ResourceModel; // Csma{edca} | SidelinkPool{subch, period, sps}
}
pub trait Dcc: Model {
    fn on_cbr(&mut self, ctx: &mut dyn Ctx, node: NodeId, cbr: f64);
    fn gate(&mut self, ctx: &mut dyn Ctx, node: NodeId, req: &TxRequest) -> GateDecision; // Now{power,mcs} | DelayUntil(t) | Drop
    fn state(&self, node: NodeId) -> DccState; // exported for HUD and metrics
}
```

Invariants: I-R1 `finish_rx` for one arrival never depends on arrivals scheduled *after* its end time (causality); I-R2 outcomes for different receivers of the same transmission are independent given the arrival set and RNG streams, so the engine may evaluate receivers in parallel (02-architecture §6.4); I-R3 every `Lost` carries exactly one cause, and the channel accounting sums to the transmitted airtime; I-R4 the abstract tier must be calibrated to the high tier of the same RAT by the validation test in 04-models §4.9 within the stated tolerance, otherwise it is registered as `uncalibrated` and the scenario validator warns.

## 5. Network, transport, infrastructure links

```rust
pub trait NetLayer: Model { // WSMP (1609.3) or GeoNetworking+BTP (EN 302 636)
    fn header_bytes(&self, meta: &NetMeta) -> u32;               // exact per standard (04-models §7)
    fn encapsulate(&self, sdu: &[u8], meta: &NetMeta) -> Result<Vec<NetPdu>, NetError>; // may yield several PDUs only if the layer legitimately fragments (GN does not; WSMP does not)
    fn decapsulate(&mut self, ctx: &mut dyn Ctx, rx: NodeId, pdu: &NetPdu) -> DecapOutcome;    // Deliver(sdu) | Forward(...) | Drop(cause)
    fn mtu(&self) -> u32;
}
pub trait Fragmenter: Model { // application-layer fragmentation strategies (04-models §7.3)
    fn fragment(&self, sdu_bytes: u32, mtu: u32) -> Vec<FragmentDesc>;
    fn reassemble(&mut self, ctx: &mut dyn Ctx, rx: NodeId, frag: &FragmentDesc, from: NodeId) -> ReassemblyOutcome; // Pending | Complete(sdu) | Failed(cause) | Expired
    fn overhead_bytes(&self) -> u32;
}
pub trait Backhaul: Model { fn send(&mut self, ctx: &mut dyn Ctx, from: NodeId, to: NodeId, bytes: u32, qos: Qos) -> SendOutcome; }
pub trait CellularUu: Model {
    fn coverage(&self, p: Vec3, t: SimTime) -> Option<CellView>;          // serving cell, RSRP-like quality class
    fn send(&mut self, ctx: &mut dyn Ctx, ue: NodeId, dir: Direction, bytes: u32, qos: Qos) -> SendOutcome; // schedules delivery with per-cell capacity/latency model; NoCoverage → caller stores & forwards
    fn handover(&mut self, ctx: &mut dyn Ctx, ue: NodeId, from: CellId, to: CellId) -> SimTime;  // interruption
}
pub trait BackendNet: Model { fn send(&mut self, ctx: &mut dyn Ctx, from: NodeId, to: NodeId, bytes: u32, qos: Qos) -> SendOutcome; fn link(&self, a: NodeId, b: NodeId) -> Option<LinkView>; }
pub enum SendOutcome { Scheduled { deliver_at: SimTime, bytes_on_wire: u32 }, Dropped(DropCause), NoCoverage }
```

Invariants: I-N1 every byte counted in `bytes_on_wire` is attributed to exactly one accounting bucket (air / cellular UL / cellular DL / backhaul / backend); I-N2 fragmentation strategies must document reassembly timeout and loss amplification in their card; I-N3 delivery events are scheduled with the class `NetDeliver` so ordering relative to other events is fixed.

## 6. Messages and security envelope

The message seam lives in `crates/v2xw-msg` and the security seam in `crates/v2xw-sec`;
those crates are the normative source for the traits below (build decision D11). The
message traits were reconciled against the crate on 2026-09-18; the security traits
(`PrimitiveDescriptor`, `CryptoBackend`, `SecurityEnvelope`) were reconciled the same day,
and the three changes that reconciliation forced are explained after the block.

```rust
pub trait MessageCodec: Model {
    fn message_types(&self) -> &[MsgType];                   // BSM, CAM, DENM, SPAT, MAP, PSM, VAM, CPM, SRM, SSM, WSA, CRL, MBR
    /// Encode to real bytes (ASN.1 UPER/COER) or, in the size-model tier, to a placeholder
    /// with an exact size from the validated size model; the result says which.
    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError>;
    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError>;
    fn supports(&self, t: MsgType) -> bool { self.message_types().contains(&t) }   // defaulted
}
pub struct Encoded { pub bytes: Vec<u8>, pub size: u32, pub size_source: SizeSource }
pub enum SizeSource { Uper, Coer, SizeModel(SizeModelVersion) }   // Coer: 1609.2/TS 103 097
pub enum Message {                                                // #[non_exhaustive]
    Cam(Box<CAM>), Denm(Box<DENM>),                               // the generated ETSI types
    HandEncoded { ty: MsgType, bytes: Vec<u8> },                  // the hand-written BSM codec
    Modeled(SizeRequest),                                         // the size-model tier
}

/// **Generic over the context and the view, not `dyn`.** The shorthand of the conventions
/// above (`&mut dyn Ctx` = `&mut dyn Ctx<World = World, Actors = ActorIndex, Payload =
/// Event>`) cannot be used here: `v2xw-msg` sits *below* the engine crate and cannot name
/// `World`, `ActorIndex` or `Event`. Parameterising the trait instead costs nothing and
/// loses nothing — with `?Sized` bounds, `C` and `V` may themselves be the trait objects,
/// so `dyn MessageGenerator<EngineCtx, EngineNodeView>` and `dyn MessageGenerator<dyn
/// Ctx<…>, dyn NodeView<…>>` are both usable, and in-process plug-ins stay trait objects
/// (ADR 0007 §8).
pub trait MessageGenerator<C: Ctx + ?Sized, V: NodeView + ?Sized>: Model {
    fn check_interval(&self) -> Duration;                         // T_CheckCamGen; the engine's timer cadence
    fn on_tick(&mut self, ctx: &mut C, node: &V, dcc: &DccState) -> Vec<GenRequest>;
    fn on_event(&mut self, ctx: &mut C, node: &V, ev: &AppEvent) -> Vec<GenRequest> { Vec::new() }
}
pub struct GenRequest { pub msg_type: MsgType, pub reason: GenReason, pub include_low_frequency: bool, pub at: SimTime }
pub enum GenReason { First, Dynamics(DynamicsTriggers), Periodic, Event }   // #[non_exhaustive]
pub struct DynamicsTriggers { pub heading: bool, pub position: bool, pub speed: bool }
/// The subset of the DCC state a generator reads. The DCC family owns the rest; this type
/// is declared in `v2xw-msg` so the message layer can be built before the DCC crate.
pub struct DccState { pub t_off: Duration, pub cbr: Option<f64> }

pub struct PrimitiveDescriptor {
    pub id: PrimitiveId,                  // a Copy newtype over &'static str, e.g. "primitive/ecdsa-p256-sha256"
    pub family: PrimitiveFamily,          // Signature | Kem | Hash | Aead | ImplicitCert
    pub pk_bytes: u32, pub pk_bytes_uncompressed: Option<u32>, pub sk_bytes: u32,
    pub sig_bytes: SizeSpec,              // Fixed(n) | Variable{mean, max} (Falcon)
    pub sig_bytes_encoded: Option<u32>,   // as carried under the 1609.2 envelope: 66 for ECDSA P-256, not 64
    pub shared_secret_bytes: Option<u32>, // for a KEM
    pub cert_bytes: Option<u32>,          // encoded certificate carrying this key under the envelope's profile
    pub security_level: u8,               // NIST level 1..5
    pub cost: CostTable,                  // per HardwareProfile model id: keygen/sign/verify cycles / µs / ops-per-s, as published, with `scaled` and a citation per row
    pub cost_components: Vec<PrimitiveId>,// a composite's cost is the sum of these (the hybrids)
    pub cost_proxy: Option<PrimitiveId>,  // whose anchors stand in when this primitive has none (ECQV -> ECDSA P-256)
    pub sources: Vec<Citation>,           // = v2xw_core::card::Source
    pub notes: Vec<&'static str>,
}

/// What a backend *is*: no simulation context, so callable without naming one.
pub trait CryptoBackendInfo: Model {
    fn mode(&self) -> CryptoMode;         // Real | Modeled
    fn backend_id(&self) -> &'static str;
    fn supports(&self, p: PrimitiveId) -> bool;
    fn public_of(&self, k: &KeyHandle) -> Result<PubHandle, SecError>;
    fn public_material(&self, pk: &PubHandle) -> Result<Vec<u8>, SecError>;  // 33 bytes, SEC 1 compressed shape, in BOTH modes (I-S1)
    fn import_public(&mut self, p: PrimitiveId, owner: NodeId, material: &[u8]) -> Result<PubHandle, SecError>;  // a peer's key, learned from a certificate
    fn hash(&self, bytes: &[u8]) -> [u8; 32] { .. }                          // real SHA-256 in both modes
    fn catalogue(&self) -> &'static PrimitiveCatalogue { .. }
    fn sig_bytes(&self, p: PrimitiveId) -> Result<u32, SecError> { .. }
    fn cost(&self, p: PrimitiveId, op: PrimitiveOpKind, profile: &str) -> Option<Duration> { .. }
}
/// What a backend *does*: the three operations that need a clock and the RNG streams.
pub trait CryptoBackend<C: Ctx + ?Sized>: CryptoBackendInfo {
    fn keygen(&mut self, ctx: &mut C, p: PrimitiveId, owner: NodeId) -> Result<KeyHandle, SecError>;
    fn sign_prehashed(&mut self, ctx: &mut C, k: &KeyHandle, digest: &[u8; 32]) -> Result<SigToken, SecError>;
    fn verify_prehashed(&mut self, ctx: &mut C, pk: &PubHandle, digest: &[u8; 32], sig: &SigToken) -> bool;
    fn sign(&mut self, ctx: &mut C, k: &KeyHandle, msg: &[u8]) -> Result<SigToken, SecError> { .. }   // defaulted: SHA-256 then sign_prehashed
    fn verify(&mut self, ctx: &mut C, pk: &PubHandle, msg: &[u8], sig: &SigToken) -> bool { .. }      // outcome identical between modes (I-S1)
}

/// The receive half: decoding and planning need no clock, so they are callable alone.
pub trait SecurityEnvelopeInfo: Model {   // IEEE 1609.2 SignedData / ETSI TS 103 097 EtsiTs103097Data
    fn profile(&self) -> EnvelopeProfile;
    fn parse(&self, bytes: &[u8]) -> Result<ParsedSecured, SecError>;
    /// A plan of primitive operations (with sizes and which store lookups) the receiver must run;
    /// the NodeRuntime charges cost and queues them. Missing certificate → P2PCD request plan.
    fn verify_plan(&self, p: &ParsedSecured, cache: &PeerCertCache, anchors: &TrustStore, crl: &CrlStore) -> VerifyPlan;
}
/// The send half. `&self`, and the backend is passed in: see note 2 below.
pub trait SecurityEnvelope<C: Ctx + ?Sized>: SecurityEnvelopeInfo {
    fn sign(&self, ctx: &mut C, crypto: &mut dyn CryptoBackend<C>, signer: &SignerHandle,
            payload: &[u8], hdr: &HeaderInfoSpec, sid: SignerIdChoice) -> Result<SecuredPdu, SecError>; // exact encoded size
}
pub trait VerificationPolicy: Model { fn admit(&mut self, ctx: &mut dyn Ctx, node: NodeId, q: &VerifyQueueView, item: &PendingVerify) -> Admit; } // Now | Defer(until) | Skip(reason) | Evict(handle)
pub trait SafetyApp: Model { fn on_neighbors(&mut self, ctx: &mut dyn Ctx, node: NodeId, t: &NeighborTable, ego: &PositionEstimate) -> Vec<Warning>; } // FCW, EEBL, IMA, VRU
```

Three notes on the shape of the security traits, all of them forced rather than chosen
(build decision D11: where the crate and this document disagree on a signature, the crate
is right and the document is corrected):

1. **Generic over the context, and split in two.** `&mut dyn Ctx` as this document
   previously published it **does not compile**: `Ctx` has three associated types with no
   defaults, so `dyn Ctx` is not a type (`rustc` E0191, checked). Parameterising over
   `C: Ctx + ?Sized` is what `MessageGenerator` above already does, and with the `?Sized`
   bound `C` may itself be a trait object, so in-process plug-ins stay trait objects
   (ADR 0007 §8). The further split into a `…Info` trait is a consequence: a method that
   does not *mention* `C` cannot live on the generic trait, because the parameter would be
   unconstrained at every call site and `backend.public_material(&pk)` would need a
   turbofish naming a context the caller never touches. The line the split falls on turns
   out to mean something — signing needs a clock and a key; parsing, planning and key
   inspection need neither.
2. **The envelope takes the backend, and takes `&self`.** A stateless envelope is `Sync`,
   so it can be called from inside the phase-parallel receive map (02-architecture §6.4);
   and the *same* envelope instance can then be driven by both crypto backends in one
   test, which is what makes the I-S1 acceptance test evidence rather than two runs.
   `SignerIdPolicy` is still the scenario-level type, but it resolves to a
   `SignerIdChoice` before `sign` is called, so the envelope holds no per-signer state.
3. **Key generation, signing and certificate building are fallible.** The post-quantum
   primitives of 04-models §9.4 deliberately have descriptors and cost tables but no
   implementation, so `Real` must be able to refuse one by name. `sign` is fallible for
   the same reason plus one more: IEEE 1609.2's `Signature` CHOICE has no post-quantum
   alternative, so a PQ signature cannot be carried under the envelope at all and is
   refused rather than mis-encoded.

Invariants: I-S1 for any run, `Real` and `Modeled` crypto modes produce identical verification outcomes, identical sizes, and identical event logs except the `crypto_mode` manifest field; a golden test runs both on the Phase 2 scenario. I-S2 `Encoded.size` is exact for `Uper` and, for `SizeModel`, within the tolerance recorded in the size model's card (04-models §8.4); I-S3 a `SecuredPdu` size equals payload + envelope overhead computed from the profile tables, and the real-encoder tier asserts equality.

## 7. Credential-management protocol

Full development in `05-protocols.md`. Summary of the seam:

```rust
pub trait CredentialProtocol: Model {
    fn id(&self) -> ProtocolId;                                   // "scms-camp-1609.2.1", "etsi-ts102941-v2", "umbrella-threshold-pq", …
    fn roles(&self) -> Vec<EntityRoleSpec>;                       // name, trust boundary, default HardwareProfile, default ServiceModel, may_be_distributed(t,n)
    fn credential_types(&self) -> Vec<CredentialTypeSpec>;        // name, encoded_bytes(fn of primitive), validity policy, lifecycle FSM, count per period
    fn flows(&self) -> Vec<FlowSpec>;                             // named message-sequence state machines over BackendNet/Uu/RSU paths
    fn primitives(&self) -> Vec<PrimitiveDescriptor>;
    fn signing_policy(&self) -> Box<dyn SigningPolicy>;           // which credential signs which MsgType; SignerIdPolicy schedule; pseudonym change strategy hook
    fn revocation(&self) -> Box<dyn RevocationMechanism>;         // Active{entry format, expansion cost, distribution paths} | Passive{starvation} | Both
    fn reporting(&self) -> Box<dyn ReportingFormat>;              // report structure, size, transport, batching
    fn trust_anchors(&self) -> Box<dyn TrustAnchorPolicy>;        // CTL/ECTL/root updates
    fn metrics(&self) -> Vec<Box<dyn MetricProvider>>;
    fn attack_hooks(&self) -> Vec<AttackHookSpec>;
    fn entity(&self, role: &EntityRoleSpec, node: NodeId) -> Box<dyn ProtocolEntity>;
    fn end_entity(&self, node: NodeId, kind: EndEntityKind) -> Box<dyn EndEntityAgent>;
}
pub trait ProtocolEntity {
    fn on_message(&mut self, ctx: &mut dyn NodeCtx, from: NodeId, msg: ProtoMessage) -> Vec<Action>;
    fn on_timer(&mut self, ctx: &mut dyn NodeCtx, timer: TimerId) -> Vec<Action>;
    fn state(&self) -> StateView;                                 // for inspector and exporters
}
pub enum Action { Send { to: NodeId, msg: ProtoMessage, bytes: u32, via: Transport }, Compute { op: OpDescriptor }, Store { delta: StoreDelta }, StartTimer { id: TimerId, at: SimTime }, Emit(EventRecord), Batch(Vec<Action>) }
```

Invariants: I-P1 every `Send` crosses a modeled link (no direct entity-to-entity calls); I-P2 every `Compute` is charged to the entity's `ServiceModel`; I-P3 a protocol declares, per message type, which credential type signs it and how the signer identifier is chosen; I-P4 revocation declares which timestamps it emits so `revocation latency by stage` (08-measurement §2.9) can be computed for any protocol.

## 8. Node runtime and hardware

```rust
pub struct HardwareProfile { /* schema in 06-node-models §1; every field cites a source */ } // addressed by HwProfileId (§1): declared once per scenario, shared by every node on that hardware
pub trait ServiceModel: Model { fn service_time(&mut self, ctx: &mut dyn Ctx, op: &OpDescriptor) -> SimTime; fn servers(&self) -> u32; fn batching(&self) -> Option<BatchPolicy>; fn availability(&self) -> Option<AvailabilityModel>; }
pub trait NodeRuntime {
    fn profile(&self) -> &HardwareProfile;
    fn submit(&mut self, ctx: &mut dyn Ctx, task: Task) -> Result<TaskHandle, Overload>;   // Task{queue: Rx|Verify|App|Tx|Crl, op: OpDescriptor, deadline}
    fn stores(&self) -> &Stores;  // CertStore, PeerCertCache, CrlStore, TrustStore, NeighborTable, EvidenceBuffer, ReportOutbox
    fn clock(&mut self, ctx: &mut dyn Ctx) -> SimTime;          // believed time
    fn position(&mut self, ctx: &mut dyn Ctx) -> &PositionEstimate;
    fn telemetry(&self) -> NodeTelemetry;                       // 06-node-models §2.9 (cpu %, ram, storage, hsm util, queue depths, drops by cause)
}
pub trait Perception: Model { fn detections(&mut self, ctx: &mut dyn Ctx, node: NodeId, ego: &Kinematics, actors: &ActorIndex, world: &World) -> Vec<Detection>; }
```

## 9. Threats, detection, response

```rust
pub struct Capabilities { pub credentials: CredentialAccess /* None | Own(n) | Stolen(set) | CompromisedRsu */, pub radio: RadioCaps { max_power_dbm, can_jam, channels }, pub knowledge: Knowledge { crl: bool, map: bool, neighbors_via_rx: bool, sensing: bool }, pub coordination: Option<CoalitionId>, pub compute: HardwareProfileRef }
pub trait Attacker: Model {
    fn capabilities(&self) -> &Capabilities;
    /// Everything the attacker can see is in the view; nothing else is reachable (I-T1).
    fn observe(&mut self, ctx: &mut dyn Ctx, view: &AttackerView);
    fn act(&mut self, ctx: &mut dyn Ctx, api: &mut AttackerApi) -> Vec<AttackAction>;   // FalsifyOutgoing{field edits before signing} | UseCredential(id) | Suppress | Delay(msg, dt) | Replay(stored) | TransmitRaw{jam profile} | ForgeReport | RsuBroadcast{fake SPaT/MAP/CRL} | Coordinate(msg)
    fn schedule(&self) -> AttackSchedule;   // duty cycle, onset jitter, geofence, wave membership
}
pub struct AttackerView { pub own_rx: &[VerifiedMessage], pub own_credentials: &[CredentialView], pub crl_public: Option<&CrlView>, pub sensed: Option<&[Detection]>, pub coalition: &[CoalitionMsg], pub own_kinematics: &PositionEstimate, pub time: SimTime }

pub trait Detector: Model {  // local, on-node
    fn on_message(&mut self, ctx: &mut dyn Ctx, node: &dyn NodeView<…>, m: &VerifiedMessage, nbrs: &NeighborTable, perc: Option<&[Detection]>) -> Vec<Observation>; // name, score in [0,1], evidence refs
    fn cost(&self) -> OpDescriptor; // charged to the node CPU
}
pub trait MaPipeline: Model {  // on the MA entity
    fn on_report(&mut self, ctx: &mut dyn NodeCtx, r: &MisbehaviorReport) -> Vec<Action>;
    fn on_timer(&mut self, ctx: &mut dyn NodeCtx, t: TimerId) -> Vec<Action>;   // windowed correlation, investigation, decision
    fn decisions(&self) -> &[Decision];
}
pub trait Responder: Model { fn on_decision(&mut self, ctx: &mut dyn NodeCtx, d: &Decision, proto: &dyn CredentialProtocol) -> Vec<Action>; }
```

Invariants: I-T1 `Attacker` implementations receive no `World` or `ActorIndex` reference; the conformance kit compiles a sentinel test that fails if the trait object can reach GT. I-T2 `Detector` and `MaPipeline` run with the node's belief only; exporters tag their outputs `NODE`. I-T3 every attack action that changes bytes on the air is logged as a GT event (`attack.action`) with the true actor id, on a GT channel only.

## 10. Measurement, export, recording

```rust
pub struct MetricDef { pub name: String, pub unit: String, pub dims: Vec<Dim> /* time, node, class, distance_bin, density_bin, region, protocol, … */, pub agg: Agg /* sum, mean, p50, p95, ratio(num, den), rate */, pub visibility: Visibility, pub definition_md: String, pub source: Option<Citation> }
pub trait MetricProvider: Model { fn defs(&self) -> Vec<MetricDef>; fn subscribe(&self) -> Vec<ChannelName>; fn on_event(&mut self, ev: &EventRecord); fn flush(&mut self, at: SimTime) -> Vec<MetricSample>; }
pub trait Exporter: Model { fn open(&mut self, run: &RunInfo) -> Result<(), ExportError>; fn on_event(&mut self, ev: &EventRecord); fn on_metric(&mut self, s: &MetricSample); fn close(&mut self) -> Result<Vec<FileDigest>, ExportError>; fn visibility(&self) -> ExporterVisibility; /* which channels it reads; GT and NODE never in the same file unless the exporter is declared `mixed` and the file is tagged */ }
pub trait Recorder { fn write(&mut self, ev: &EventRecord); fn keyframe(&mut self, snap: &WorldSnapshot); fn finish(&mut self) -> RecordingIndex; }
// `EventRecord` is the recorded form of a plug-in's `Record` (§1.1), and both live in
// `v2xw_core::ctx`:
pub struct ChannelName(pub &'static str);  // Ord + Hash + Display; `ChannelName::of::<R>()`
pub struct OwnedRecord { pub channel: &'static str, pub visibility: Visibility, pub json: Vec<u8> }
pub type EventRecord = OwnedRecord;
// What reaches the engine from `Ctx::emit` is a `&dyn ErasedRecord`, carrying the channel id
// and the visibility tag, with `write_json` as the universal fallback and `as_any` for the
// per-channel fast path. That borrow dies with the call, so a recorder or a metric provider
// that batches, shards or queues calls `ErasedRecord::to_owned_record() -> Result<OwnedRecord>`
// once and keeps the result. `ChannelName` is the *recording* channel; §4's `ChannelId` is a
// 5.9 GHz channel number, and the two must not be confused.
```

## 11. UI-side renderer contract

Not a plug-in in the engine, but a documented interface in `ui/packages/viewer`: `Renderer.applySnapshot(keyframe)`, `Renderer.applyDelta(delta)`, `Renderer.setOverlay(name, on)`, `Renderer.follow(nodeId, cameraMode)`, `Renderer.sampleAt(t)` (pose interpolation between the last two deltas), and `Inspector.explain(subject)` which resolves provenance ids delivered by the stream into (model card, parameter values). See `09-ui.md`.

## 12. Model card schema (mandatory for every plug-in)

JSON Schema (draft 2020-12), abbreviated:

```yaml
$id: https://v2xw.dev/schema/model-card-1.json
type: object
required: [id, family, version, tier, purpose, equations, parameters, assumptions, limitations, sources, validation, api_version]
properties:
  id:            {type: string, pattern: "^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$"}   # e.g. "radio/propagation/log-distance-shadowing"
  family:        {enum: [world, mobility, vru, weather, gnss, clock, propagation, fading, obstacle, phy, mac, dcc, net, fragmenter, backhaul, cellular, backend-net, codec, generator, envelope, primitive, crypto-backend, verification-policy, safety-app, protocol, service-model, hardware-profile, perception, attacker, detector, ma-pipeline, responder, metric, exporter]}
  version:       {type: string, description: semver}
  api_version:   {type: string, description: plug-in API semver the card targets}
  tier:          {type: array, minItems: 1, items: {enum: [abstract, medium, high]}}   # required, and may not be empty
  purpose:       {type: string}
  equations:     {type: array, items: {type: object, required: [name, latex_or_text], properties: {name: {type: string}, latex_or_text: {type: string}, notes: {type: string}}}}
  parameters:    {type: array, items: {type: object, required: [name, unit, default, source], properties:
                    {name: {type: string}, unit: {type: string}, default: {}, range: {type: array}, source: {$ref: "#/$defs/source"}, calibration: {type: string, description: "TODO: calibrate plan when source is 'todo-calibrate'"}}}}
  assumptions:   {type: array, items: {type: string}}
  limitations:   {type: array, items: {type: string}}
  ignores:       {type: array, items: {type: string}, description: "what this tier leaves out relative to the next tier up"}
  sources:       {type: array, items: {$ref: "#/$defs/source"}}
  validation:    {type: object, required: [status], properties: {status: {enum: [unvalidated, unit-tested, literature-checked, field-checked]}, references: {type: array, items: {$ref: "#/$defs/source"}}, tests: {type: array, items: {type: string}}}}
  determinism:   {type: object, properties: {uses_rng: {type: boolean}, rng_domains: {type: array, items: {type: string}}}}
  cost:          {type: object, properties: {per_call_us: {type: number}, notes: {type: string}}, description: "optional: computational cost class for the scheduler and the tier planner"}
$defs:
  source:
    type: object
    required: [kind, ref]
    properties:
      kind: {enum: [standard, paper, datasheet, dataset, code, todo-calibrate]}
      ref:  {type: string, description: "standard+clause, DOI/URL, datasheet URL"}
      accessed: {type: string, format: date}
      note: {type: string}
```

Registry rules: (R1) a parameter whose `source.kind` is `todo-calibrate` must carry a `calibration` plan; the generated docs list all such parameters on one page; (R2) a scenario referencing a model with `validation.status = unvalidated` in a `high` tier gets a validator warning that is written to the manifest; (R3) the docs site is generated from cards, never hand-written for models.

### What the crate enforces, and where

The Rust form of this schema is `v2xw_core::card::ModelCard` (§1.2). Three different stages check a card, and it is worth knowing which one rejects what:

| Stage | Check | Failure |
|---|---|---|
| `ModelCard::validate` | `id` matches `^[a-z0-9][a-z0-9-]*(/[a-z0-9-]+)*$` | `CardError::InvalidId` |
| | `version`, `api_version`, `purpose` non-empty | `CardError::EmptyField` |
| | **`tier` non-empty** | `CardError::EmptyField { field: "tier" }` |
| | parameter names unique | `CardError::DuplicateParameter` |
| | rule R1: a `todo-calibrate` source carries a calibration plan | `CardError::MissingCalibration` |
| | **each parameter's `default` satisfies its own declared `range`** | `CardError::DefaultOutOfRange` |
| `ModelCard::check_api_version` | the card's `api_version` majors match the engine's `API_VERSION` (currently `1.0.0`); for major `0` the minors must match too | `CardError::ApiVersionMismatch` |
| `Registry::register*` | validate, then API version, then duplicate `id`, then the licence gate of ADR 0007 §5 (copyleft and unknown licences may only be hosted out of process); nothing is stored if any of them fails | `RegistryError::{Duplicate, LicenceRequiresOutOfProcess, InvalidCard, Unhashable}` |
| `ParamSet::resolve` | invariant I-C3 at scenario level: every overridden name is declared, the JSON types match, and the declared `range` holds | `RegistryError::{UnknownParameter, ParameterType, ParameterRange, BadOverrides}` |

The range is checked at **both** stages, and deliberately so: a scenario that does not mention a parameter — the common case — never reaches the resolver's check, because the default is copied in verbatim. A card whose default contradicted its own range therefore used to run on the value the card declares impossible, with the check on overrides giving false assurance. Both stages use one pair of helpers, so they cannot disagree about what a declared range means: two numeric entries are the inclusive interval `[min, max]`, anything else is the set of allowed values, and **numbers are compared numerically** — `1` and `1.0` are the same value, matching the type check, which calls integers and floats one type because every authoring format spells them interchangeably.

`tier` is not decoration: the scenario selects one tier per family, so a card that declares none describes a model that no scenario can select and that `implements_tier` answers `false` for at every tier. The empty list is rejected at registration rather than at scenario load so that the failure names the plug-in. `ModelCard::new` therefore seeds `tier` with `[medium]`, the tier `medium` itself documents as "the default"; a model implementing another tier, or several, overwrites the field.

A card's **content hash** is SHA-256 over its canonical JSON (compact, map keys sorted at every level), which is what the manifest pins. It must not depend on formatting, field declaration order, or which features are enabled in the dependency graph, which is why the canonical encoder exists rather than `serde_json::to_vec`.

One divergence between this schema and the Rust deserialiser is recorded rather than resolved: `assumptions`, `limitations`, `sources` and `validation` are in the schema's `required` list, but `ModelCard` defaults all four, so a card omitting them deserialises successfully and only the schema validator would object. Whether the fields are genuinely mandatory (and the Rust type should stop defaulting them) or merely strongly expected is a decision for a human, not for whichever of the two happens to be read first.

## 13. Scenario schema outline

One YAML/JSON file (schema `scenario-1.json`, published with help text and units for every field). Top level:

```yaml
schema: v2xw/scenario/1
meta: {name, description, authors, tags, created, base: <preset id or path>}      # base → overlay merge
seed: 0x…                                     # master seed; sub-streams derived, never user-set
time: {t0: "2027-03-04T07:00:00Z", duration_s: 600, mobility_step_ms: 100, des_resolution: 1us}
world: {source: {kind: osm|sumo|procedural|json|editor, …}, buildings: {…}, terrain: {…}, cache: …}
actors:
  vehicles: {demand: {kind: od|arrival|flow, …}, classes: {car: {fraction, dims, obu: profile-id|null}, truck: …}, equipped_fraction}
  vru: {pedestrians: {…}, cyclists: {…}, devices: {psm|vam, fraction}}
  rsus: [{site, roles: [crl, provisioning-proxy, report-forward, spat, map, wsa], profile, backhaul}]
  cellular: {cells: […] | coverage_map, uu_model}
  backend: {protocol: <id>, entities: {ra: {profile, service_model, net}, pca: …, la1, la2, ma, crlg, …}, topology: {links: […]}}
weather: {initial, fronts: via events}
radio:
  rat: dsrc-80211p | lte-v2x-pc5 | nr-v2x-pc5 | hybrid
  tiers: {propagation: high, phy: medium, mac: medium, focus: {region: follow:<node>|bbox, tier: high}}
  models: {propagation: {id, params}, fading: {…}, obstacle: {…}, phy: {…}, mac: {…}, dcc: {…}}
net: {layer: wsmp|gn-btp, fragmenter: {…}, backhaul: {…}, uu: {…}, backend_net: {…}}
messages: {sets: [bsm|cam, denm, spat, map, psm|vam, cpm], generator: {…}, codec: {tier: uper|size-model}}
security:
  envelope: ieee1609.2 | etsi103097
  protocol: {id: scms-camp | etsi-ts102941 | umbrella-threshold-pq, params: {…}}
  primitives: {signature: ecdsa-p256 | ml-dsa-65 | …, hybrid: …}
  crypto_mode: modeled | real
  verification_policy: verify-all | on-demand | prioritized
  signer_id_policy: {full_cert_every_ms: 1000|450, digest_otherwise: true}
  pseudonym_change: {strategy: time|distance|mix-zone|silent, params}
nodes: {profiles: {default_obu: <profile id>, per_class: {…}}, tiers: {compute: medium, backend: medium}}
threats: {attackers: [{id, fraction|count|ids, capabilities, params, schedule}], jammers: […], compromised_rsus: […]}
detection: {local: [{id, params}], ma: {pipeline id, params}, responder: {…}, perception: {tier, params}}
metrics: [ids or "all"]
exporters: [{id: ma-dataset-v2, opts}, {id: receiver-logs}, {id: telemetry}, {id: net-trace}, {id: recording, keyframe_s: 1}]
events:                                       # timeline; each item {t, until?, type, params}
  - {t: 120, until: 300, type: demand.multiplier, value: 3}
  - {t: 200, type: weather.front, value: fog}
  - {t: 250, type: attack.wave, ids: [...]}
  - {t: 400, type: outage, target: 12, until: 460}          # a node id
  - {t: 450, until: 600, type: closure, target: "edge:314"}  # or lane:<id>, street:<name>
  - {t: 500, type: param.change, path: security.pseudonym_change.period_s, value: 120}
experiment: null | {sweep: {...}, seeds: [...], replications: n}   # see 08-measurement §4
```

**The timeline as built (2026-09-23).** Every item is a control-priority event (priority 0), so
everything at that instant sees it, and writes a `scenario.event` record of what it did
(`crates/v2xw-engine/src/timeline.rs`):

| `type` | Effect | `until` |
|---|---|---|
| `weather.front` | the weather drivers and links see (`value`, `intensity`, `visibility_m`, `surface`) | not allowed: the next front replaces it |
| `outage` | node `target` stops transmitting and receiving | the node comes back |
| `demand.multiplier` | the thinned-Poisson arrival rate × `value`; several compose by product; the candidate process is sized to the timeline's peak so the thinning stays exact | the multiplier is lifted |
| `closure` | the vehicle lanes of `target` (`lane:<id>`, `edge:<id>`, `street:<name>`) cost infinity to every router; every vehicle re-plans; one already on a closed lane finishes it; one that cannot avoid it leaves the run at the barrier (`RouteBlocked`) | the lanes reopen |
| `param.change` | one parameter from `LIVE_PARAMS` takes `value`: `weather.*` and `actors.vehicles.demand.rate_veh_per_h` now; the equipped and device shares, `security.verification_policy`, `security.pseudonym_change.*` and `nodes.default_obu` for what enters after the change; any other path is refused at load, because the model that reads it is built when the run starts | not allowed |
| `attack.wave` | the attacker populations `ids` (index or model id) act only inside the wave: the wave is their `AttackSchedule`, one window per population | the population goes quiet |

A closure target that names no vehicle lane is refused when the run is built, naming the item.

Validation returns actionable errors (`radio.tiers.phy: 'high' requires mac 'high' (mac is 'medium')`) and migration is by explicit `schema` version with a migrator per version step.

## 14. Event log schema (recording channels)

Recorded as MCAP channels; each channel has a self-describing schema record, a visibility tag, and a stable id. The recording carries **two encodings**, on two topic namespaces, and one topic never carries both (12-build-decisions D11 item 5): `snapshot.keyframe` and `snapshot.delta` store the VWP binary frames of `docs/protocol/vwp-v1.md` verbatim, header included, because storing the bytes that went over the wire is what makes the live and replay streams provably identical; every other channel below takes the serde `Record` path and lands in Parquet or JSONL, where a self-describing columnar format is worth far more than zero copy. A plug-in reaches this table through one Rust type per channel implementing `Record` (§1.1), whose `CHANNEL` constant is the id in the first column and whose `VISIBILITY` constant is the tag in the second. The visibility column below is written in prose; the canonical spellings, which are what a record serialises with, are `gt`, `node`, `node-and-gt`, `public`, `derived`, `mixed` and `meta`. The engine consumes records through `ErasedRecord`: `write_json` is the fallback every recorder can implement, and `as_any` lets a recorder that knows the channel downcast to the concrete type and take its columnar path instead of paying for JSON on a hot path. Families (full list in `08-measurement-and-data.md` §5):

| Channel | Visibility | Key fields |
|---|---|---|
| `gt.kinematics` | GT | t, actor, pos, vel, acc, heading, lane |
| `gt.attack.action` | GT | t, actor, attacker id, action, fields changed |
| `gt.spawn` / `gt.despawn` | GT | t, actor, class, cause |
| `node.tx` | NODE | t, node, msg type, bytes, mcs, power, channel, ac, dcc state, pseudonym digest |
| `phy.rx` | NODE+GT | t_start, t_end, tx, rx, rssi, sinr, outcome, cause (the tx id is GT; exporters project it out for NODE-only outputs) |
| `phy.prr` | GT | t, tx, msg, msg type, bins: `[20 m bin, receivers truly in range, receivers that decoded]` — the per-frame census behind the 3GPP packet reception ratio (TR 36.885 §A.2.1.4); record-only, never an `Event` payload |
| `mac.cbr` | NODE | t, node, channel, cbr |
| `net.frag` | NODE | t, node, sdu id, fragments, outcome |
| `net.reassembly` | GT | t, rx, tx, sdu, strategy, kind (message / segments / certificate), msg type, fragments, received, bytes, bytes received, predicted loss `1 − Π(1 − p_i)` and predicted content loss from the fragments' PHY success probabilities, outcome, cause — one fragmented SDU followed to its fate at one receiver (04-models.md §7.4); record-only, never an `Event` payload |
| `node.verify` | NODE | t_enqueue, t_start, t_done, node, primitive, cost µs, outcome, policy decision |
| `node.telemetry` | NODE | t, node, cpu %, ram, storage, hsm util, queue depths, drops by cause |
| `node.neighbor` | NODE | t, node, table delta |
| `sec.cert` | NODE | t, node, event (change, expire, top-up, learn), digest |
| `proto.msg` | NODE | t, from, to, flow, step, bytes, transport |
| `proto.revocation` | PUBLIC | t, stage, id, size |
| `scenario.event` | PUBLIC | t, index, kind, phase (start/end), effect, lanes, multiplier, path, value, populations |
| `det.observation` | NODE | t, node, detector, subject digest, score |
| `ma.report` / `ma.case` / `ma.decision` | NODE | as in the MA dataset |
| `app.warning` | NODE | t, node, app, subject, kind |
| `metric.sample` | derived | t, name, dims, value |
| `snapshot.keyframe` / `snapshot.delta` | mixed (UI) | world state for the UI, stored as the verbatim VWP frames rather than as serde records (see above); GT-tagged parts are stripped by the `NODE-only` replay profile |
| `manifest` | meta | one record: full manifest |

## 15. Python SDK (batched) equivalents

The Python SDK mirrors each trait with **per-step batches**, never per-frame calls, so that Python plug-ins remain viable up to ~10,000 nodes in the abstract tier:

```python
class Detector(v2xw.plugins.Detector):
    card = ModelCard(...)                                   # same schema as §12, validated on import
    def on_messages(self, ctx: Ctx, node: NodeView, batch: pa.RecordBatch) -> list[Observation]: ...
class Attacker(v2xw.plugins.Attacker):
    def act(self, ctx: Ctx, view: AttackerView, api: AttackerApi) -> list[AttackAction]: ...
class ProtocolEntity(v2xw.plugins.ProtocolEntity):
    def on_messages(self, ctx: NodeCtx, msgs: list[ProtoMessage]) -> list[Action]: ...
class MetricProvider(v2xw.plugins.MetricProvider): ...
class Exporter(v2xw.plugins.Exporter): ...
```

Rules: (P1) Python plug-ins are allowed for `attacker`, `detector`, `ma-pipeline`, `responder`, `protocol` (entities and flows), `metric`, `exporter`, `generator` (application-level), `service-model`, `demand`, `world` importers, `safety-app`; (P2) `propagation`, `fading`, `phy`, `mac`, `dcc`, `car-following`, `lane-change` accept Python only at `abstract` tier (batched per step) and the registry marks such runs `python-hot-path` in the manifest with an expected slowdown; (P3) the SDK ships typed stubs and a `v2xw plugin new <family>` scaffold with a passing conformance test.

## 16. Out-of-process plug-ins

gRPC (protobuf, Apache-2.0) service per family with the same method set; used for SUMO (via TraCI adapter inside the engine process, so SUMO is not itself a gRPC plug-in), ns-3 cross-validation harness, and third-party plug-ins in other languages. Determinism: the engine sends the entity RNG stream seed with each batch (`RngRegistry::ephemeral`, §1.1, which derives an uncached stream at word zero for exactly this case); the remote side must be pure given inputs, and the engine hashes replies into the run digest so a nondeterministic remote is detected by the golden test. An out-of-tree plug-in's own RNG domain and entity scope are derived from its model card id (`RngDomain::plugin`, `EntityRef::custom`) rather than hand-picked, so two independently written plug-ins collide only on a hash collision of their ids.

## 17. Conformance test kit (per interface)

Each family has a `conformance/<family>.rs|py` suite that any plug-in must pass to be listed in the registry:

- determinism: two runs, identical outputs (I-C1);
- thread-count independence: the same phase run as a serial loop and as a parallel map over 8 threads draws byte-identical values. This is the property `Ctx::rng`'s `&self` accessor and the per-entity stream keying exist for (§1.1), and `v2xw-core` pins it for the registry itself;
- dyn-compatibility: the suite builds the plug-in behind `Box<dyn Family>` and `Arc<dyn Model + Send + Sync>`, and calls it through `&mut dyn Ctx<…>`. If a change to `Ctx` or `Model` reintroduced a generic method, this is where it fails rather than in the nine crates downstream;
- card validation: `ModelCard::validate` passes (id pattern, non-empty `tier`, unique parameter names, rule R1) and `check_api_version` accepts the card against this engine build (§12);
- card completeness: every parameter read at runtime is declared (I-C3), checked by a params-access tracer, and every RNG domain drawn from is declared in `determinism.rng_domains`;
- quantisation: every float in the plug-in's emitted records sits on its declared grid, scanned with `math::is_on_grid` (build decision D9). A value off its grid fails the build;
- recorder discipline: no record whose `Visibility::is_gt_tainted` reaches a NODE channel, which is `Visibility::allowed_on_node_channel` as a test;
- stream hygiene: a plug-in that draws per transmission uses a single-use scope (`EntityRef::LinkFrame`) rather than interning a stream per frame, so the registry's cache stays bounded by the number of live entities;
- tier contract: implements the declared tier(s) and answers `ignores` for lower tiers;
- family-specific: e.g., `Phy` must conserve airtime accounting (I-R3), `Propagation` must be monotone non-decreasing in distance for LOS free-space inputs, `Fragmenter` must reassemble in order and report loss amplification, `CryptoBackend` must satisfy I-S1, `Attacker` must compile against the sentinel (I-T1), `Exporter` must pass the leakage linter on its outputs;
- performance: a per-call budget in the card (`cost`) checked by a microbenchmark in CI with a regression threshold.
