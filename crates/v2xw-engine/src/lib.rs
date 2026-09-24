//! `v2xw-engine` — the simulation kernel: the scenario, the event enum, the main loop, the
//! manifest.
//!
//! Build decision D8 created this crate and said what it owns. It sits above
//! `v2xw-node` and below `v2xw-server`, `v2xw-cli` and `v2xw-py`, and it is where the
//! nine model crates stop being libraries and start being a run.
//!
//! | What D8 assigns here | Module |
//! |---|---|
//! | the scenario schema and loader | [`scenario`] |
//! | the concrete `Event` enum that instantiates `Scheduler<E>` | [`event`] |
//! | the world, actor and node state, and the main loop | [`run`] |
//! | the phase-parallel structure of ADR 0004 | [`run`] |
//! | the run manifest assembly | [`manifest`] |
//! | the normative `Keyframe`/`Delta` wire stream | [`snapshot`] |
//!
//! Two more modules exist because the layering demands them rather than because the
//! decision listed them: [`ctx`] is the one implementation of
//! [`v2xw_core::ctx::Ctx`] in the system — it is what binds the three associated types the
//! contract crate leaves open — and [`adapters`] is the per-family narrowing build
//! decision D12.2 requires the engine to supply.
//!
//! # The shortest possible run
//!
//! ```no_run
//! use v2xw_engine::{Engine, MemoryRecorder, Scenario};
//!
//! let scenario = Scenario::load("scenarios/grid-traffic.yaml")?;
//! // The build timestamp is the caller's: no part of the engine may read a wall clock
//! // (02-architecture.md §6.1), and the field is excluded from every digest.
//! let mut engine = Engine::build(scenario, "2026-09-22T00:00:00Z")?;
//! let mut recorder = MemoryRecorder::new();
//! let report = engine.run(&mut recorder)?;
//! println!("{} frames, {} records", report.frames_transmitted, report.records);
//! # Ok::<(), v2xw_engine::EngineError>(())
//! ```
//!
//! # What the determinism contract costs here
//!
//! Every rule the contract crate states has a consequence in this crate, and each is worth
//! knowing where to find:
//!
//! * **No wall clock.** [`Engine::build`] takes the manifest timestamp as an argument and
//!   the scenario carries `world.imported_at`; there is no `SystemTime` call anywhere.
//! * **All randomness from keyed streams.** Whether a vehicle is equipped is drawn from
//!   `(Spawn, Actor)`, and a reception outcome from `(AbstractRx, LinkFrame)` — both keys
//!   that make the draw independent of what else happened first.
//! * **No hash iteration reaching an output.** Every collection the run iterates to produce
//!   an ordering is a `BTreeMap` or an explicitly sorted `Vec`; the two merges say so in
//!   code rather than relying on the source's iteration order.
//! * **Quantisation at the writer.** [`records`] quantises in the constructor, so a caller
//!   cannot emit an unquantised float by forgetting to.
//! * **Phase-parallel only.** The event loop is a single `while let Some(..) = pop()`. The
//!   parallel map is in [`Engine::on_phy_end`](run::Engine); nothing else uses `rayon`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod adapters;
pub mod ctx;
pub mod error;
pub mod event;
pub mod export;
pub mod manifest;
pub mod phase2;
pub mod records;
pub mod run;
pub mod scenario;
pub mod snapshot;
pub mod wiring;

pub use ctx::{DigestRecorder, EngineCtx, MemoryRecorder, NullRecorder, RunRecorder};
pub use error::{EngineError, Result, ScenarioError};
pub use event::{Event, NodeTask, Observe};
pub use run::{Engine, RunReport};
pub use scenario::Scenario;
pub use snapshot::{ActorState, SnapshotStream, snapshot_cadence};
