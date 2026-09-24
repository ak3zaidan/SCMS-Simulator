//! `v2xw-server` — the control surface and the stream: JSON-RPC control plus the VWP
//! WebSocket of `docs/protocol/vwp-v1.md`.
//!
//! This crate is the transport a browser attaches to. It serves one run — live from an
//! engine or replayed from an MCAP recording — over one WebSocket endpoint, with the
//! JSON-RPC control plane of §6 on the text channel of the same socket and on `POST /rpc`.
//!
//! # What is here
//!
//! | Concern | Module | Specification |
//! |---|---|---|
//! | HTTP router, the upgrade, the connection task, the producer | [`http`] | §1.1–§1.3, §1.5 |
//! | The connection state machine, resume, subscriptions, framing | [`session`] | §1.3, §1.4, §2.5, §5.3, §6.7, §6.12 |
//! | The resume ring and the priority send queue | [`ring`] | §1.4, §1.5 |
//! | The 32 methods and the 8 notifications | [`rpc`] | §6 |
//! | The OpenRPC 1.3.2 document | [`openrpc`] | §6.3 |
//! | The engine seam | [`engine`] | — |
//! | A live `v2xw-engine` run | [`live`] | — |
//! | A deterministic synthetic engine | [`stub`] | — |
//! | Replay from a recording | [`replay`] | §7 |
//! | The `{path, message, hint}` error surface | [`error`] | §6.4 |
//!
//! # One encoder
//!
//! Every VWP frame this crate sends is built by `v2xw-record`'s encoder, and every frame
//! it replays is one `v2xw-record`'s reader handed back. There is no second implementation
//! of the §3 layouts in this crate, on purpose: two encoders drift, and §7.2's
//! byte-identity guarantee is only provable if there is one path.
//!
//! The one exception is the three *connection* frames — `Hello` is built by `v2xw-record`,
//! but `Error` (§3.10) and `Bye` (§3.11) are written here, in [`http::error_frame`] and
//! [`http::bye_frame`], because they are connection-scoped and are never recorded, so the
//! recording crate has no encoder for them.
//!
//! # The rules this crate is written under
//!
//! * **No std transcendental.** Every one goes through [`v2xw_core::math`].
//! * **No wall clock below the transport.** [`http`] reads one, because §1.2's ping
//!   interval, §1.5's stall timeout and live pacing are wall-clock quantities by
//!   definition. Nothing else in the crate does, and no simulated or recorded value is
//!   derived from it.
//! * **No `std` `HashMap` iteration reaches an output.** Every table that decides what a
//!   frame or a JSON result contains is a `BTreeMap`, a `BTreeSet` or an explicitly sorted
//!   `Vec`.
//! * **No randomness.** The fixture engine has none at all (see [`stub`]); the transport
//!   has none to have.
//! * No `unsafe`, and every public item documented.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod feed;
pub mod http;
pub mod introspect;
pub mod live;
pub mod openrpc;
pub mod replay;
pub mod resume;
pub mod ring;
pub mod rpc;
pub mod run;
pub mod session;
pub mod stub;

use std::net::SocketAddr;
use std::sync::Arc;

pub use engine::{Engine, NodeFacts, RunDescriptor, RunState, StepOutput};
pub use error::{ParamError, Result, ServerError};
pub use live::{LiveEngine, LiveOptions};
pub use replay::ReplayEngine;
pub use run::Run;
pub use session::{ConnectParams, FeedSub, Session};
pub use stub::{StubEngine, StubOptions};

/// The numeric `Visibility` code of §3.1.5 for a channel's tag.
pub fn visibility_code(v: v2xw_core::Visibility) -> u8 {
    match v {
        v2xw_core::Visibility::Gt => 0,
        v2xw_core::Visibility::Node => 1,
        v2xw_core::Visibility::Public => 2,
        // §3.1.5's code 3 is `MIXED`, which is what `NodeAndGt` and `Mixed` both are on
        // the wire: a record with node-visible and ground-truth columns side by side.
        v2xw_core::Visibility::NodeAndGt | v2xw_core::Visibility::Mixed => 3,
        v2xw_core::Visibility::Derived => 4,
        v2xw_core::Visibility::Meta => 5,
        // `Visibility` is `#[non_exhaustive]`. A tag this build does not know is reported
        // as `GT`, the strictest one: a new class must never be treated as node-visible by
        // accident, because that is exactly how a blind evaluation leaks.
        _ => 0,
    }
}

/// The `#/$defs/Visibility` token of §6.5 for a channel's tag.
pub fn visibility_name(v: v2xw_core::Visibility) -> &'static str {
    match v {
        v2xw_core::Visibility::Gt => "GT",
        v2xw_core::Visibility::Node => "NODE",
        v2xw_core::Visibility::Public => "PUBLIC",
        v2xw_core::Visibility::NodeAndGt | v2xw_core::Visibility::Mixed => "MIXED",
        v2xw_core::Visibility::Derived => "DERIVED",
        v2xw_core::Visibility::Meta => "META",
        // See `visibility_code`: unknown means strictest.
        _ => "GT",
    }
}

/// How to start a server.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// The address to bind. Loopback by default (02-architecture §12).
    pub bind: SocketAddr,
    /// The bearer token a non-loopback bind requires. `None` on loopback.
    pub token: Option<String>,
    /// A token interned into the run's own symbol table up front.
    ///
    /// Not the token a `Hello` issues any more: every session gets its own, minted by
    /// [`resume::Sessions`], because §1.4's resume names *a session* and one token shared
    /// by every connection could not. Kept so a caller that pinned the run's table size
    /// keeps compiling; `""` is the right value.
    pub session_token: String,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            bind: SocketAddr::from(([127, 0, 0, 1], 8787)),
            token: None,
            session_token: String::new(),
        }
    }
}

/// A bound, running server.
#[derive(Debug)]
pub struct VwpServer {
    address: SocketAddr,
    run: Arc<Run>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    producer: tokio::task::JoinHandle<()>,
}

impl VwpServer {
    /// Binds and starts serving `run`.
    ///
    /// A non-loopback bind without a token is refused rather than served open, per
    /// 02-architecture §12: an engine reachable from the network with no authentication is
    /// a mistake that is easy to make by passing `--host 0.0.0.0` and hard to notice.
    ///
    /// # Errors
    /// [`ServerError::Io`] if the address cannot be bound, or
    /// [`ServerError::Unauthorized`] for a non-loopback bind with no token.
    pub async fn start(run: Arc<Run>, options: ServerOptions) -> Result<Self> {
        if !options.bind.ip().is_loopback() && options.token.is_none() {
            return Err(ServerError::Unauthorized);
        }
        let state = http::AppState {
            run: Arc::clone(&run),
            token: options.token.map(Arc::new),
            sessions: Arc::new(resume::Sessions::new()),
        };
        let app = http::router(state);
        let listener = tokio::net::TcpListener::bind(options.bind)
            .await
            .map_err(|e| ServerError::Io {
                path: options.bind.to_string(),
                errno: e.to_string(),
            })?;
        let address = listener.local_addr().map_err(|e| ServerError::Io {
            path: options.bind.to_string(),
            errno: e.to_string(),
        })?;
        let (shutdown, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        let producer = tokio::spawn(http::producer(Arc::clone(&run)));
        Ok(VwpServer {
            address,
            run,
            shutdown,
            task,
            producer,
        })
    }

    /// The bound address, which is what a caller that passed port `0` needs.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The HTTP origin, e.g. `http://127.0.0.1:8787`.
    pub fn http_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The WebSocket endpoint, e.g. `ws://127.0.0.1:8787/vwp/v1`.
    pub fn ws_url(&self) -> String {
        format!("ws://{}/vwp/v1", self.address)
    }

    /// The run being served.
    pub fn run(&self) -> &Arc<Run> {
        &self.run
    }

    /// Stops serving and waits for the tasks to finish.
    pub async fn stop(self) {
        let _ = self.shutdown.send(());
        self.producer.abort();
        let _ = self.task.await;
    }
}

/// Starts a server on `bind` running `scenario` on the real engine.
///
/// The scenario is loaded, the world is imported and the kernel is built before this
/// returns, so a caller that gets an `Ok` has a run: the banner it prints names a world
/// hash and a run id that exist. A scenario that does not load, or a world that does not
/// import, is an error here rather than a server listening on a run that failed.
///
/// # Errors
/// Whatever the scenario loader, the world importer, the kernel or the bind refuses.
pub async fn serve_scenario(
    options: ServerOptions,
    scenario: impl AsRef<std::path::Path>,
    mut live: LiveOptions,
) -> Result<VwpServer> {
    live.session_token = options.session_token.clone();
    let engine = LiveEngine::open(scenario, live)?;
    let world_json = engine.world_json().to_string();
    let run = Run::new(Box::new(engine), world_json)?;
    VwpServer::start(run, options).await
}

/// Starts a server on `bind` with the synthetic fixture engine.
///
/// Kept for a client developer who wants a stream with no scenario, no city extract and
/// no import: it is a real implementation of the [`Engine`] seam on a procedurally
/// generated grid. Every number it reports is a fixture value — see [`stub`].
///
/// # Errors
/// Whatever the fixture or the bind refuses.
pub async fn serve_stub(options: ServerOptions, stub: StubOptions) -> Result<VwpServer> {
    let engine = StubEngine::new(stub)?;
    let world_json = v2xw_world::serde_vwp::to_json_string(engine.geometry())?;
    let run = Run::new(Box::new(engine), world_json)?;
    VwpServer::start(run, options).await
}

/// Starts a server that replays a recording (§7).
///
/// # Errors
/// [`ServerError::Io`] if the recording cannot be read, or whatever the bind refuses.
pub async fn serve_replay(
    options: ServerOptions,
    recording: impl AsRef<std::path::Path>,
    world: Arc<v2xw_world::WorldPayload>,
    world_json: String,
) -> Result<VwpServer> {
    let engine = ReplayEngine::open(recording, world)?;
    let run = Run::new(Box::new(engine), world_json)?;
    VwpServer::start(run, options).await
}
