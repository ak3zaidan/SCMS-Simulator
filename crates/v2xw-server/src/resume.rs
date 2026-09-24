//! Sessions that outlive their socket (§1.4).
//!
//! A [`Session`] used to die with its WebSocket, so a reconnect after a two-second network
//! drop found nothing to resume: it got a fresh `Hello`, a resync keyframe and an empty
//! symbol table, and a page that had been following a car lost the car. §1.4 describes the
//! other behaviour — a reconnect that names its session and the `seq` it expects next gets
//! exactly the frames it missed — and this module is what makes that possible.
//!
//! # The shape
//!
//! Every session is owned by exactly one task at a time: the connection task while a socket
//! is attached, a *parked* task while none is. The parked task keeps doing what the
//! connection did — it takes every step from the run's broadcast and encodes it — except
//! that the frames go only into the session's resume ring. So the ring holds the frames the
//! client missed, numbered with the `seq` it would have seen, and a resume is a replay of
//! the ring from the client's `seq` with nothing re-encoded and nothing renumbered.
//!
//! Ownership moves by message, never by shared lock: the registry maps a token to the
//! sending half of a claim channel, and whoever owns the session holds the receiving half
//! *inside* the hand-over value, so the channel travels with the session. A claim is a
//! one-shot reply slot; the owner answers it by giving the session away. When the owner is a
//! live connection, that is §1.4's supersede rule: the old socket gets `Bye{reason = 4}` and
//! close 1012, and the new one carries on the same stream.
//!
//! # Bounds
//!
//! A parked session costs its ring (at most 8 MiB, §1.4) and the CPU of encoding one run
//! for nobody. Both are bounded: a session stays parked for [`RESUME_TTL`] of wall time and
//! no more than [`MAX_PARKED`] are parked at once, the oldest evicted first. A client that
//! closes cleanly (close code 1000) is not parked at all — it said it is not coming back.
//!
//! # Wall clock
//!
//! The TTL is wall time, like every other transport timer in [`crate::http`]; no simulated
//! quantity depends on it. The instant a session was parked, and the instant a claim is
//! made, are read in `http.rs` — the one server module allowed to read a clock — and handed
//! in, as `rpc::Context::received_at` is.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration as WallDuration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::engine::StepOutput;
use crate::run::Run;
use crate::session::Session;

/// How long a session whose socket went away waits to be resumed.
///
/// Long enough to cover a laptop lid, a Wi-Fi hand-over or a proxy restart; short enough
/// that a closed tab's session does not encode a run for nobody for long. §1.4's client
/// backoff reaches its 8 s cap after five attempts, so 120 s is about fifteen attempts.
pub const RESUME_TTL: WallDuration = WallDuration::from_secs(120);

/// How many sessions may be parked at once. Each costs up to 8 MiB of ring (§1.4).
pub const MAX_PARKED: usize = 8;

/// How long a reconnect waits for the current owner to give a session up.
pub const CLAIM_TIMEOUT: WallDuration = WallDuration::from_secs(2);

/// A request for a session: the owner answers it by sending the session.
pub type Claim = oneshot::Sender<Box<Detached>>;

/// A session in transit between owners, with everything it needs to carry on.
#[derive(Debug)]
pub struct Detached {
    /// The session: profile, subscriptions, encoder, ring, `seq`.
    pub session: Session,
    /// Its subscription to the run's steps. Moving it — rather than subscribing afresh — is
    /// what makes the hand-over lossless: a step broadcast between the old socket dying and
    /// the new one attaching is still queued on it.
    pub steps: broadcast::Receiver<Arc<StepOutput>>,
    /// Its subscription to run-scoped notifications, which queue while parked and are
    /// delivered after the replay.
    pub notices: broadcast::Receiver<Value>,
    /// The receiving half of this session's claim channel.
    pub claims: mpsc::Receiver<Claim>,
    /// The run generation of the last `Hello` the *client* was sent. A parked session that
    /// followed the run into a new generation has a `Hello` its client never saw, so it
    /// cannot be resumed — only regreeted.
    pub client_generation: u64,
}

#[derive(Debug)]
struct Entry {
    claim: mpsc::Sender<Claim>,
    /// Distinguishes this registration from a later one under the same token.
    epoch: u64,
    /// When it was parked; `None` while a socket owns it.
    parked_at: Option<Instant>,
}

/// The server's sessions, by token.
#[derive(Debug)]
pub struct Sessions {
    entries: Mutex<BTreeMap<String, Entry>>,
    counter: AtomicU64,
    salt: [u8; 32],
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}

impl Sessions {
    /// An empty registry with a fresh salt for its tokens.
    pub fn new() -> Self {
        Sessions {
            entries: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
            salt: salt(),
        }
    }

    /// A new, unguessable token.
    ///
    /// The token is a capability: whoever presents it takes the session over, including
    /// its follow and its event subscriptions. On a non-loopback bind the bearer token
    /// already gates the socket, so this only has to be unguessable by another client of
    /// the same server — 128 bits of SHA-256 over a per-process random salt and a counter.
    pub fn mint(&self) -> String {
        use sha2::Digest;
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut h = sha2::Sha256::new();
        h.update(self.salt);
        h.update(n.to_le_bytes());
        let digest = h.finalize();
        digest[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Registers a session under `token`, owned by a socket, and returns the receiving half
    /// of its claim channel.
    pub fn register(&self, token: &str) -> mpsc::Receiver<Claim> {
        let (tx, rx) = mpsc::channel(4);
        let epoch = self.counter.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().insert(
            token.to_string(),
            Entry {
                claim: tx,
                epoch,
                parked_at: None,
            },
        );
        rx
    }

    /// Asks whoever owns `token`'s session to hand it over.
    ///
    /// `None` when there is no such session, or its owner did not answer within
    /// [`CLAIM_TIMEOUT`] — in either case the caller starts a fresh session, which is
    /// §1.4 rule 2 and never an error.
    pub async fn claim(&self, token: &str, now: Instant) -> Option<Box<Detached>> {
        self.expire(now);
        let sender = self.entries.lock().get(token).map(|e| e.claim.clone())?;
        let (reply, answer) = oneshot::channel();
        if sender.send(reply).await.is_err() {
            return None;
        }
        let detached = tokio::time::timeout(CLAIM_TIMEOUT, answer)
            .await
            .ok()?
            .ok()?;
        if let Some(entry) = self.entries.lock().get_mut(token) {
            entry.parked_at = None;
        }
        Some(detached)
    }

    /// Forgets `token`, for a session nobody may resume (a clean close).
    pub fn forget(&self, token: &str) {
        self.entries.lock().remove(token);
    }

    /// How many sessions are parked now.
    pub fn parked(&self) -> usize {
        self.entries
            .lock()
            .values()
            .filter(|e| e.parked_at.is_some())
            .count()
    }

    /// How many sessions are registered, parked or attached.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// True when no session is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }

    /// Parks a session whose socket went away, until it is claimed or expires.
    ///
    /// Spawns the task that keeps its ring current. Evicts the oldest parked session when
    /// [`MAX_PARKED`] would be exceeded; an evicted session's task ends when its claim
    /// channel closes.
    pub fn park(self: &Arc<Self>, run: Arc<Run>, detached: Box<Detached>, now: Instant) {
        let token = detached.session.session_token().to_string();
        let epoch = {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.get_mut(&token) else {
                // Forgotten while the socket was closing: nothing may resume it.
                return;
            };
            entry.parked_at = Some(now);
            let epoch = entry.epoch;
            let mut parked: Vec<(Instant, String)> = entries
                .iter()
                .filter_map(|(t, e)| e.parked_at.map(|at| (at, t.clone())))
                .collect();
            parked.sort();
            while parked.len() > MAX_PARKED {
                let (_, oldest) = parked.remove(0);
                entries.remove(&oldest);
            }
            epoch
        };
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            parked_task(registry, run, token, epoch, detached).await;
        });
    }

    /// Drops every parked session parked [`RESUME_TTL`] or more before `now`.
    fn expire(&self, now: Instant) {
        self.entries.lock().retain(|_, e| {
            e.parked_at
                .is_none_or(|at| now.saturating_duration_since(at) < RESUME_TTL)
        });
    }

    fn remove_if(&self, token: &str, epoch: u64) {
        let mut entries = self.entries.lock();
        if entries.get(token).is_some_and(|e| e.epoch == epoch) {
            entries.remove(token);
        }
    }
}

/// The parked owner: encodes every step into the ring until claimed or expired.
async fn parked_task(
    registry: Arc<Sessions>,
    run: Arc<Run>,
    token: String,
    epoch: u64,
    mut d: Box<Detached>,
) {
    let expiry = tokio::time::sleep(RESUME_TTL);
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            biased;
            claim = d.claims.recv() => {
                match claim {
                    Some(reply) => {
                        // If the claimant has gone, keep waiting for the next one.
                        match reply.send(d) {
                            Ok(()) => return,
                            Err(back) => d = back,
                        }
                    }
                    // Evicted, or the server is going away.
                    None => return,
                }
            }
            step = d.steps.recv() => {
                match step {
                    Ok(output) => {
                        if output.generation < d.session.generation() {
                            continue;
                        }
                        if output.generation > d.session.generation()
                            && d.session.regreet(&run).is_err()
                        {
                            registry.remove_if(&token, epoch);
                            return;
                        }
                        // The frames go to the ring and nowhere else: this is the stream
                        // the client would have seen, kept for when it comes back.
                        if d.session.encode_step(&output).is_err() {
                            registry.remove_if(&token, epoch);
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Steps were lost before they were encoded, so the next frame in
                        // the ring must re-seed the client: a resync keyframe, exactly as
                        // a live connection that lagged gets one (§1.5).
                        d.session.request_resync();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        registry.remove_if(&token, epoch);
                        return;
                    }
                }
            }
            () = &mut expiry => {
                registry.remove_if(&token, epoch);
                return;
            }
        }
    }
}

/// 32 bytes the process cannot predict: the OS generator where there is one, and the
/// standard library's per-process hash keys (themselves seeded from the OS) otherwise.
fn salt() -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    let mut buf = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    h.update(buf);
    {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        h.update(hasher.finish().to_le_bytes());
    }
    h.finalize().into()
}
