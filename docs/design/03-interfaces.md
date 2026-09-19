# 03 — Plug-in interfaces, data types, invariants, and schemas

Status: design draft for review (2026-09-17). Companion to `02-architecture.md`. Every interface here is a plug-in seam; the model families behind each seam are catalogued in `04-models.md`, the protocol seam is developed in `05-protocols.md`, and node internals in `06-node-models.md`.

Conventions used in this document:

- Signatures are given in Rust because the engine core is Rust (ADR 0003). The Python SDK exposes the same interfaces with batched calls (§15). Out-of-process plug-ins speak the same messages over gRPC (§16).
- `Tier` is `Abstract | Medium | High` everywhere. A plug-in declares the tier(s) it implements in its model card; the scenario selects a tier per family.
- Every trait extends `Model`, which supplies the model card (§12). A plug-in without a model card cannot be registered.
- "Invariants" are checked by the conformance test kit (§17). A plug-in that violates one fails registration in CI.
- Units: metres, seconds (as `SimTime` ns ticks internally), radians, dBm/dB, bytes, Hz. Fields carry their unit in the name when ambiguous (`_m`, `_s`, `_ns`, `_dbm`, `_db`, `_hz`, `_bytes`).

## 1. Core types

```rust
/// Nanoseconds since scenario start t0. u64 gives 584 years of range; the engine
/// guarantees 1 µs resolution to models (ADR 0004).
pub type SimTime = u64;
pub const NS_PER_US: SimTime = 1_000;
pub const NS_PER_MS: SimTime = 1_000_000;
pub const NS_PER_S:  SimTime = 1_000_000_000;

/// Typed ids. All are dense u32 indices assigned at scenario load in a
/// deterministic order (sorted by scenario declaration, then by spawn time, then by
/// spawn sequence). Ids are never reused within a run.
pub struct ActorId(pub u32);      // vehicle, pedestrian, cyclist (physical thing)
pub struct NodeId(pub u32);       // OBU, VRU device, RSU, BS, router, backend entity
pub struct LaneId(pub u32);
pub struct EdgeId(pub u32);
pub struct JunctionId(pub u32);
pub struct SignalId(pub u32);
pub struct BuildingId(pub u32);
pub struct CellId(pub u32);       // cellular cell
pub struct LinkKey(pub NodeId, pub NodeId); // ordered (tx, rx) for per-link fading state

/// World-local East-North-Up metres. World origin (lat, lon, alt) is a world field.
pub struct Vec3 { pub x: f64, pub y: f64, pub z: f64 }

pub struct LanePos { pub lane: LaneId, pub s_m: f64, pub d_m: f64 } // longitudinal, lateral offset

/// Ground-truth kinematic state. Produced by Mobility at each mobility step.
pub struct Kinematics {
    pub t: SimTime,
    pub pos: Vec3,            // reference point = rear-axle centre for vehicles, centroid for VRU
    pub vel: Vec3,            // m/s
    pub acc: Vec3,            // m/s²
    pub heading_rad: f64,     // ENU, 0 = east, counter-clockwise
    pub yaw_rate_rad_s: f64,
    pub lane: Option<LanePos>,
    pub dims: Dims,           // length, width, height (m)
}

pub enum Tier { Abstract, Medium, High }

/// Total order of events: (time, priority, seq). `seq` is assigned by the scheduler at
/// schedule() time from a single monotonic counter, so two events at the same time
/// and priority run in the order they were scheduled. Priorities are fixed per event
/// class (02-architecture §5.3), not chosen by plug-ins.
pub struct EventKey { pub time: SimTime, pub priority: u8, pub seq: u64 }
```

### 1.1 Engine context handed to every plug-in call

```rust
pub trait Ctx {
    fn now(&self) -> SimTime;
    /// Per-entity, per-domain deterministic RNG stream (02-architecture §6.2). A plug-in
    /// never owns an RNG; it asks for the stream of the entity it is acting for.
    fn rng(&mut self, domain: RngDomain, entity: EntityRef) -> &mut RngStream;
    fn schedule(&mut self, at: SimTime, class: EventClass, payload: EventPayload) -> EventHandle;
    fn cancel(&mut self, h: EventHandle) -> bool;
    fn world(&self) -> &World;
    fn actors(&self) -> &ActorIndex;           // spatial index over current kinematics
    /// Typed telemetry/event emission into the recorder (13 Event log schema). Records
    /// carry a visibility tag; the recorder refuses GT records on NODE channels.
    fn emit<R: Record>(&mut self, r: R);
    /// Provenance: attach (model, version, parameters) to a value the UI or an exporter
    /// may show. Cheap: it records a ModelRef id and a parameter-set id, not copies.
    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId);
    /// Resolved configuration for this plug-in instance (defaults merged with scenario).
    fn params(&self) -> &ParamSet;
}

pub struct RngStream(/* ChaCha12 keyed by (master_seed, domain, entity), counter-indexed */);
impl RngStream { pub fn u64(&mut self) -> u64; pub fn f64(&mut self) -> f64; pub fn normal(&mut self, mu: f64, sigma: f64) -> f64; pub fn exp(&mut self, rate: f64) -> f64; pub fn nakagami(&mut self, m: f64, omega: f64) -> f64; /* … all distributions implemented in-crate with fixed algorithms, never from platform libm */ }
```

Invariants (all plug-ins):

- I-C1 A plug-in may not keep wall-clock time, thread ids, or process-global RNG state. Conformance kit runs the plug-in twice and compares emitted records byte for byte.
- I-C2 A plug-in may not read `World` ground truth for a node's *belief* unless the interface explicitly passes it (e.g., `Mobility` gets GT; `Detector` does not). The Python SDK enforces this by handing `Detector` a `NodeView`, not the world.
- I-C3 Every numeric parameter a plug-in reads must be declared in its model card with unit, default, and source, or the registry rejects it (§12).

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
pub struct PositionEstimate { pub pos: Vec3, pub vel: Vec3, pub heading_rad: f64, pub semi_major_m: f64, pub semi_minor_m: f64, pub orientation_rad: f64, pub time_ns: SimTime, pub fix: FixQuality }
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

```rust
pub trait MessageCodec: Model {
    fn message_types(&self) -> &[MsgType];                   // BSM, CAM, DENM, SPAT, MAP, PSM, VAM, CPM, SRM, SSM, WSA, CRL, MBR, …
    /// Encode to real bytes (ASN.1 UPER) or, in the size-model tier, to a placeholder with an
    /// exact size from the validated size model; the result says which.
    fn encode(&self, msg: &Message) -> Result<Encoded, CodecError>;
    fn decode(&self, bytes: &[u8], t: MsgType) -> Result<Message, CodecError>;
}
pub struct Encoded { pub bytes: Vec<u8>, pub size: u32, pub size_source: SizeSource /* Uper | SizeModel(version) */ }

pub trait MessageGenerator: Model { // J2945/1 BSM rules, EN 302 637-2 CAM rules, DENM repetition, SPaT/MAP, PSM/VAM, CPM
    fn on_tick(&mut self, ctx: &mut dyn Ctx, node: &NodeView, dcc: &DccState) -> Vec<GenRequest>; // called at the generator's own cadence via its timers
    fn on_event(&mut self, ctx: &mut dyn Ctx, node: &NodeView, ev: &AppEvent) -> Vec<GenRequest>; // e.g., hazard → DENM
}

pub struct PrimitiveDescriptor {
    pub id: PrimitiveId,                  // e.g. "ecdsa-p256-sha256", "ml-dsa-65", "falcon-512", "slh-dsa-shake-128s", "ecqv-p256"
    pub family: PrimitiveFamily,          // Signature | Kem | Hash | Aead | ImplicitCert
    pub pk_bytes: u32, pub sk_bytes: u32,
    pub sig_bytes: SizeSpec,              // Fixed(n) | Variable{mean, max} (Falcon)
    pub cert_bytes: Option<u32>,          // encoded certificate carrying this key under the envelope's profile
    pub security_level: u8,               // NIST level 1..5
    pub cost: CostTable,                  // per HardwareProfile id: keygen/sign/verify µs (mean, p95), cycles, where run (cpu|hsm|accel), sources
    pub sources: Vec<Citation>,
}
pub trait CryptoBackend: Model {
    fn mode(&self) -> CryptoMode;         // Real | Modeled
    fn keygen(&mut self, ctx: &mut dyn Ctx, p: PrimitiveId, owner: NodeId) -> KeyHandle;
    fn sign(&mut self, ctx: &mut dyn Ctx, k: KeyHandle, msg: &[u8]) -> SigToken;        // Real: real bytes; Modeled: token {key id, msg hash}
    fn verify(&mut self, ctx: &mut dyn Ctx, pk: PubHandle, msg: &[u8], sig: &SigToken) -> bool; // outcome identical between modes (I-S1)
}
pub trait SecurityEnvelope: Model { // IEEE 1609.2 SignedData / ETSI TS 103 097 EtsiTs103097Data
    fn profile(&self) -> EnvelopeProfile;
    fn sign(&mut self, ctx: &mut dyn Ctx, signer: &SignerHandle, payload: &[u8], hdr: &HeaderInfo, sid: SignerIdPolicy) -> Result<SecuredPdu, SecError>; // exact encoded size
    fn parse(&self, bytes: &[u8]) -> Result<ParsedSecured, SecError>;
    /// A plan of primitive operations (with sizes and which store lookups) the receiver must run;
    /// the NodeRuntime charges cost and queues them. Missing certificate → P2PCD request plan.
    fn verify_plan(&self, p: &ParsedSecured, cache: &PeerCertCache, anchors: &TrustStore, crl: &CrlStore) -> VerifyPlan;
}
pub trait VerificationPolicy: Model { fn admit(&mut self, ctx: &mut dyn Ctx, node: NodeId, q: &VerifyQueueView, item: &PendingVerify) -> Admit; } // Now | Defer(until) | Skip(reason) | Evict(handle)
pub trait SafetyApp: Model { fn on_neighbors(&mut self, ctx: &mut dyn Ctx, node: NodeId, t: &NeighborTable, ego: &PositionEstimate) -> Vec<Warning>; } // FCW, EEBL, IMA, VRU
```

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
pub struct HardwareProfile { /* schema in 06-node-models §1; every field cites a source */ }
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
    fn on_message(&mut self, ctx: &mut dyn Ctx, node: &NodeView, m: &VerifiedMessage, nbrs: &NeighborTable, perc: Option<&[Detection]>) -> Vec<Observation>; // name, score in [0,1], evidence refs
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
pub trait MetricProvider: Model { fn defs(&self) -> Vec<MetricDef>; fn subscribe(&self) -> Vec<ChannelId>; fn on_event(&mut self, ev: &EventRecord); fn flush(&mut self, at: SimTime) -> Vec<MetricSample>; }
pub trait Exporter: Model { fn open(&mut self, run: &RunInfo) -> Result<(), ExportError>; fn on_event(&mut self, ev: &EventRecord); fn on_metric(&mut self, s: &MetricSample); fn close(&mut self) -> Result<Vec<FileDigest>, ExportError>; fn visibility(&self) -> ExporterVisibility; /* which channels it reads; GT and NODE never in the same file unless the exporter is declared `mixed` and the file is tagged */ }
pub trait Recorder { fn write(&mut self, ev: &EventRecord); fn keyframe(&mut self, snap: &WorldSnapshot); fn finish(&mut self) -> RecordingIndex; }
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
  tier:          {type: array, items: {enum: [abstract, medium, high]}}
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
  cost:          {type: object, description: "optional: computational cost class for the scheduler (per-call µs estimate)"}
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
  - {t: 400, type: outage, target: backend.ra, until: 460}
  - {t: 500, type: param.change, path: security.pseudonym_change.params.period_s, value: 120}
experiment: null | {sweep: {...}, seeds: [...], replications: n}   # see 08-measurement §4
```

Validation returns actionable errors (`radio.tiers.phy: 'high' requires mac 'high' (mac is 'medium')`) and migration is by explicit `schema` version with a migrator per version step.

## 14. Event log schema (recording channels)

Recorded as MCAP channels; each channel has a FlatBuffers schema, a visibility tag, and a stable id. Families (full list in `08-measurement-and-data.md` §5):

| Channel | Visibility | Key fields |
|---|---|---|
| `gt.kinematics` | GT | t, actor, pos, vel, acc, heading, lane |
| `gt.attack.action` | GT | t, actor, attacker id, action, fields changed |
| `gt.spawn` / `gt.despawn` | GT | t, actor, class, cause |
| `node.tx` | NODE | t, node, msg type, bytes, mcs, power, channel, ac, dcc state, pseudonym digest |
| `phy.rx` | NODE+GT | t_start, t_end, tx, rx, rssi, sinr, outcome, cause (the tx id is GT; exporters project it out for NODE-only outputs) |
| `mac.cbr` | NODE | t, node, channel, cbr |
| `net.frag` | NODE | t, node, sdu id, fragments, outcome |
| `node.verify` | NODE | t_enqueue, t_start, t_done, node, primitive, cost µs, outcome, policy decision |
| `node.telemetry` | NODE | t, node, cpu %, ram, storage, hsm util, queue depths, drops by cause |
| `node.neighbor` | NODE | t, node, table delta |
| `sec.cert` | NODE | t, node, event (change, expire, top-up, learn), digest |
| `proto.msg` | NODE | t, from, to, flow, step, bytes, transport |
| `proto.revocation` | PUBLIC | t, stage, id, size |
| `det.observation` | NODE | t, node, detector, subject digest, score |
| `ma.report` / `ma.case` / `ma.decision` | NODE | as in the MA dataset |
| `app.warning` | NODE | t, node, app, subject, kind |
| `metric.sample` | derived | t, name, dims, value |
| `snapshot.keyframe` / `snapshot.delta` | mixed (UI) | world state for the UI; GT-tagged parts are stripped by the `NODE-only` replay profile |
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

gRPC (protobuf, Apache-2.0) service per family with the same method set; used for SUMO (via TraCI adapter inside the engine process, so SUMO is not itself a gRPC plug-in), ns-3 cross-validation harness, and third-party plug-ins in other languages. Determinism: the engine sends the entity RNG stream seed with each batch; the remote side must be pure given inputs, and the engine hashes replies into the run digest so a nondeterministic remote is detected by the golden test.

## 17. Conformance test kit (per interface)

Each family has a `conformance/<family>.rs|py` suite that any plug-in must pass to be listed in the registry:

- determinism: two runs, identical outputs (I-C1);
- card completeness: every parameter read at runtime is declared (I-C3), checked by a params-access tracer;
- tier contract: implements the declared tier(s) and answers `ignores` for lower tiers;
- family-specific: e.g., `Phy` must conserve airtime accounting (I-R3), `Propagation` must be monotone non-decreasing in distance for LOS free-space inputs, `Fragmenter` must reassemble in order and report loss amplification, `CryptoBackend` must satisfy I-S1, `Attacker` must compile against the sentinel (I-T1), `Exporter` must pass the leakage linter on its outputs;
- performance: a per-call budget in the card (`cost`) checked by a microbenchmark in CI with a regression threshold.
