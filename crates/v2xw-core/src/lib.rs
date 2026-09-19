//! `v2xw-core` — the contract crate of the V2X World Simulator.
//!
//! Every other `v2xw-*` crate compiles against this one. It holds the discrete-event
//! kernel (clock, event heap), the deterministic random-number streams, the model
//! registry and model-card schema, the provenance log and the run manifest. It holds
//! no domain models: no propagation equations, no car-following, no protocol logic
//! (02-architecture.md §2, ADR 0010).
//!
//! # The determinism contract
//!
//! Same scenario file + seed + engine build + plug-in set ⇒ byte-identical outputs on
//! macOS/Linux/Windows, x86-64/arm64, native or WASM, single- or multi-threaded
//! (02-architecture.md §6.1). This crate is where the four mechanisms that make that
//! true are implemented:
//!
//! 1. **Counter-based RNG** ([`rng`]). There is no shared sequential generator. Every
//!    draw comes from a [`RngStream`] keyed by `SHA-256(master_seed ‖ domain ‖ entity)`,
//!    so one entity's draw sequence never depends on another entity's activity, on the
//!    event order, or on the thread count (ADR 0004 §3, 02-architecture.md §6.2).
//!    Every distribution sampler is implemented in this crate with a documented, fixed
//!    algorithm and a fixed draw count.
//! 2. **Pure-Rust transcendentals** ([`math`]). `f64::sin` and friends call the platform
//!    libm, whose precision the Rust standard library documents as varying "by platform,
//!    Rust version, and can even differ within the same execution". Every transcendental
//!    in the engine therefore goes through [`math`], which re-exports the `libm` crate
//!    (ADR 0003, ADR 0004 §4). `sqrt` is exempt: IEEE-754 makes it exact.
//! 3. **Fixed event priorities** ([`event`]). The scheduler's total order is
//!    `(time, priority, seq)` with a priority fixed per [`EventClass`] and a globally
//!    monotonic `seq` assigned at schedule time, so ties are resolved by a documented
//!    rule rather than by chance (02-architecture.md §5.1).
//! 4. **Id-ordered reductions**. Ids ([`ids`]) are dense, deterministically assigned and
//!    totally ordered; every parallel phase merges its results in id order and every
//!    reduction sorts its contributors by id before summing (02-architecture.md §6.4).
//!    This crate supplies the ordered id types; the merging happens in the phase crates.
//! 5. **Writer-side quantisation** ([`math::quantize_to`], ADR 0004 decision 7, build
//!    decision D9). No float reaches a recorded, exported or digested artefact in raw
//!    IEEE-754 form: every one is rounded to its field's declared quantum first, so a
//!    digest survives a change of math library, compiler or target. The ADR's own evidence
//!    is a legacy corpus whose golden digests broke because exactly one field escaped
//!    `round(x, 3)`. [`math::is_on_grid`] is the predicate the output-scanning test uses.
//!
//! The parallelism those rules are written for is phase-parallel
//! (02-architecture.md §6.4): pure maps over actors, receivers or nodes, merged in id
//! order, with a single-threaded event loop. Everything a model touches inside such a map
//! — [`rng::RngRegistry::checkout`], [`ctx::Ctx::rng`] — therefore takes `&self`, and the
//! crate's own tests run a phase on 1 and on 8 threads and compare the drawn values byte
//! for byte.
//!
//! Two further rules are enforced by convention and by CI rather than by the type system:
//! no plug-in may read wall-clock time (the single exception in this crate is the
//! caller-supplied [`Manifest::build_utc`] string, which is excluded from every digest),
//! and no crate may call `f64::sin`-style standard-library transcendentals.
//!
//! # Where to look
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | Simulated time, durations, wall clock, IEEE 1609.2 time | [`time`] | 03-interfaces.md §1, ADR 0004 |
//! | Typed ids | [`ids`] | 03-interfaces.md §1 |
//! | Vectors, lane positions, bounding boxes | [`geom`] | 03-interfaces.md §1 |
//! | The geodetic origin and the ENU projection | [`geo`] | build decision D6 |
//! | Uniform-grid cell arithmetic and neighbourhood order | [`grid`] | ADR 0004 §6, 02-architecture.md §5.2 |
//! | Ground-truth kinematics and the extrapolation rule | [`kinematics`] | 02-architecture.md §5.2 |
//! | A node's *belief* about its position and time | [`belief`] | 03-interfaces.md §3 |
//! | Weather state and its driving effects | [`weather`] | 03-interfaces.md §3, §4 |
//! | Deterministic transcendentals, the quantiser, ordered reductions | [`math`] | ADR 0003, ADR 0004 §4, §7 |
//! | SHA-256 helpers, streaming digest, canonical JSON | [`hash`] | 02-architecture.md §6.5 |
//! | RNG domains, streams, registry | [`rng`] | ADR 0004 §3, 02-architecture.md §6.2 |
//! | Event classes, keys, scheduler | [`event`] | 02-architecture.md §5.1 |
//! | The plug-in context and recorded records | [`ctx`] | 03-interfaces.md §1.1, §14 |
//! | The base trait every family extends | [`model`] | 03-interfaces.md conventions |
//! | The belief-only node handle (invariant I-C2) | [`nodeview`] | 03-interfaces.md §1, §9 |
//! | Model cards | [`card`] | 03-interfaces.md §12 |
//! | Serde codecs for the in-band float sentinels | [`serde_sentinel`] | 03-interfaces.md §1, §3 |
//! | Model registry and parameter sets | [`registry`] | ADR 0007 |
//! | Provenance ("why" panel) | [`provenance`] | 02-architecture.md §6.5 |
//! | Run manifest and data digest | [`manifest`] | 02-architecture.md §6.5 |
//! | Errors | [`error`] | — |
//!
//! # The two firewalls
//!
//! Two of those rows are not conveniences but enforcement, and a crate that works around
//! them breaks a published invariant rather than a style rule:
//!
//! * [`nodeview::NodeView`] is invariant **I-C2**. A plug-in that runs as a node — a
//!   detector, a message generator, an attacker — is handed one of these and never a
//!   [`Ctx`]'s world, so it cannot read the ground truth its own result is supposed to be
//!   measured against.
//! * [`math::quantize_to`] is ADR 0004 decision 7. Every float that reaches a recorded,
//!   exported or digested artefact goes through it first, and
//!   [`belief::PositionEstimate::quantized`], [`weather::WeatherState::quantized`] and
//!   their neighbours are the per-type spellings of that rule.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod belief;
pub mod card;
pub mod ctx;
pub mod error;
pub mod event;
pub mod geo;
pub mod geom;
pub mod grid;
pub mod hash;
pub mod ids;
pub mod kinematics;
pub mod manifest;
pub mod math;
pub mod model;
pub mod nodeview;
pub mod provenance;
pub mod registry;
pub mod rng;
pub mod serde_sentinel;
pub mod time;
pub mod weather;

pub use belief::{FixQuality, PositionEstimate};
pub use card::{
    API_VERSION, CardError, CostClass, Determinism, Equation, Family, ModelCard, Parameter, Source,
    SourceKind, Tier, Validation, ValidationStatus,
};
pub use ctx::{
    ChannelName, Ctx, CtxExt, ErasedRecord, EventRecord, OwnedRecord, Record, Visibility,
};
pub use error::{CoreError, Result};
pub use event::{EventClass, EventHandle, EventKey, Scheduler};
pub use geo::GeoOrigin;
pub use geom::{Bbox, Dims, LanePos, Vec3};
pub use grid::{GridCell, GridIndex};
pub use hash::{Sha256Writer, canonical_json, hex_encode, sha256, sha256_hex};
pub use ids::{
    ActorId, BuildingId, CellId, ConnectionId, CrossingId, EdgeId, FrameSeq, HwProfileId,
    JunctionId, LanduseId, LaneId, LinkKey, NodeId, SduId, SignalId, SiteId,
};
pub use kinematics::Kinematics;
pub use manifest::{CryptoMode, FileDigest, Manifest, PluginPin, TimeDilationWindow};
pub use math::{
    grid_index, is_on_grid, is_quantized, q3, quantile_sorted, quantize, quantize_to,
    sort_total_order, sum_ordered, sum_sorted_by_key,
};
pub use model::{Model, ModelHandle};
pub use nodeview::NodeView;
pub use provenance::{GeometryKind, ProvSubject, ProvenanceEntry, ProvenanceLog};
pub use registry::{
    Hosting, Licence, ModelRef, ParamSet, ParamSetId, ParamSetStore, RegisteredModel, Registry,
    RegistryError,
};
pub use rng::{EntityRef, PluginDomain, RngDomain, RngGuard, RngRegistry, RngStream};
pub use time::{
    CivilDateTime, Duration, IEEE1609_EPOCH_UNIX_S, NS_PER_MS, NS_PER_S, NS_PER_US, SimTime,
    TimeError, WallClock, format_sim_time, ns_to_secs, secs_to_ns,
};
pub use weather::{DrivingEffects, SurfaceCondition, WeatherKind, WeatherState};
