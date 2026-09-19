//! Counter-based deterministic random-number streams.
//!
//! There is **no shared sequential generator** in the engine. Every draw comes from a
//! [`RngStream`]: a ChaCha12 keystream whose 32-byte key is
//! `SHA-256(master_seed ‖ domain ‖ entity)` (ADR 0004 §3, 02-architecture.md §6.2).
//! Because the key depends only on *who is drawing and what for*, one entity's draw
//! sequence is independent of every other entity's activity, of the order in which
//! events fire, and of the thread count. This is the generalisation of the legacy
//! engine's per-vehicle string-keyed streams (`f"{seed}:sensor:{vid}"`, 01-inventory §3),
//! and it removes their order dependence.
//!
//! A plug-in never owns an RNG. It asks the engine context for the stream of the entity
//! it is acting for ([`crate::ctx::Ctx::rng`], 03-interfaces.md §1.1), which resolves to
//! [`RngRegistry::checkout`].
//!
//! # Key derivation (stable; changing it invalidates every golden digest)
//!
//! ```text
//! key = SHA-256( master_seed_le(8) ‖ domain_code_le(4) ‖ entity_encoding )
//! ```
//!
//! with `entity_encoding` as documented on [`EntityRef::encode`] and `domain_code` as
//! documented on [`RngDomain::code`].
//!
//! # One accessor, usable from a parallel phase
//!
//! Parallelism in this engine is phase-parallel: pure maps over actors, receivers or
//! nodes, merged in id order (02-architecture.md §6.4, ADR 0004 decision 5). Draws happen
//! *inside* those maps, so the registry accessor must take `&self`:
//! [`RngRegistry::checkout`] does, and is the one sanctioned way to draw. It hands out an
//! [`RngGuard`] that borrows the stream out of a sharded, individually locked cache and
//! returns it on drop, so
//!
//! * the value sequence of a key is identical whether the draw happened on the event loop
//!   or inside a `rayon` map, with any thread count (`parallel_phase_is_thread_count_independent`
//!   in this module's tests pins it byte for byte for 1 and 8 threads);
//! * a second task that asks for a key already checked out **panics** instead of silently
//!   receiving a stream restarted at word 0.
//!
//! [`RngRegistry::stream`] is the same cache reached through `&mut self`, for the
//! single-threaded event loop where no locking is wanted. Both paths share one cache, so
//! they cannot disagree. [`RngStream::derive`] and [`RngRegistry::ephemeral`] bypass the
//! cache and are documented traps: they restart a stream at word 0 every time.
//!
//! # Two scope lifetimes
//!
//! Keys come in two kinds, distinguished by [`EntityRef::is_single_use`]:
//!
//! * **long-lived** (`Global`, `Actor`, `Node`, `Link`, `Lane`, `Signal`, `Custom`) — the
//!   stream is cached, so consecutive draws for one key continue the same keystream;
//! * **single-use** ([`EntityRef::LinkFrame`], whose key already embeds a per-frame
//!   counter) — the stream is derived, used and dropped. Caching those would intern one
//!   ~300-byte generator per frame per directed link: at the design target (10,000 nodes,
//!   10 Hz, ~50 neighbours, 60 s) some 3 × 10⁸ streams, tens of gigabytes.
//!
//! [`RngRegistry::checkout`] routes on that flag by itself, so a call site cannot get it
//! wrong; [`RngRegistry::stream`] rejects single-use scopes in debug builds because it
//! has nowhere to put a stream it must not cache.
//!
//! # One domain, one purpose
//!
//! Order-independence is a property of a *key*, not of an entity: if two models draw from
//! the same `(domain, entity)` key their draws interleave, and each model's sequence then
//! depends on which model ran first. A domain therefore names one purpose — this is why
//! [`RngDomain::DesiredSpeed`] and [`RngDomain::ReactionTime`] are separate from
//! [`RngDomain::Mobility`] — and an out-of-tree plug-in's domain is derived from its model
//! id ([`PluginDomain::for_model`]) rather than picked by hand, so two independently
//! written plug-ins cannot land on the same code.
//!
//! # Samplers
//!
//! Every distribution is implemented here, with a fixed algorithm and a documented draw
//! count, using [`crate::math`] for transcendentals — never the platform libm and never
//! the `rand` crate's distributions, whose algorithms may change between versions
//! (02-architecture.md §6.2).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use rand_chacha::ChaCha12Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::hash::sha256;
use crate::ids::{ActorId, LaneId, LinkKey, NodeId, SignalId};
use crate::math;

/// An out-of-tree plug-in's RNG domain code, derived from the plug-in's model id.
///
/// There is no allocation authority for hand-picked plug-in domain numbers, so there is no
/// hand-picking: the code is `0x8000_0000 | (LE_u32(SHA-256(model_id)[0..4]) >> 1)`. Two
/// independently written plug-ins collide only on a 31-bit hash collision of their model
/// ids (probability ≈ 2⁻³¹ per pair), instead of colliding whenever both authors happen to
/// pick `1`. The high bit is always set, so a plug-in code can never equal a built-in one.
///
/// The inner code is deliberately not constructible from an integer in code; the
/// `Deserialize` impl accepts one so that a recorded card or manifest round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginDomain(u32);

impl PluginDomain {
    /// Derives the domain code of the plug-in whose model card id is `model_id`.
    ///
    /// The id is the card's `id` field (`radio/fading/my-model`), not its display name,
    /// because that is what the registry and the manifest pin.
    pub fn for_model(model_id: &str) -> Self {
        let h = sha256(model_id.as_bytes());
        let raw = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        Self(0x8000_0000 | (raw >> 1))
    }

    /// The numeric code mixed into the stream key.
    pub const fn code(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for PluginDomain {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "plugin-{:08x}", self.0)
    }
}

/// What a draw is *for*.
///
/// The domain is part of the stream key, so the shadowing draws of a link cannot shift
/// its fading draws, and adding a new model that draws from a new domain cannot perturb
/// the streams of existing models.
///
/// **One domain names one purpose.** Two models that draw from the same `(domain, entity)`
/// key interleave their draws, and each then depends on the order the other ran in — the
/// very order dependence this module exists to remove. Splitting a purpose out into its
/// own domain is always safe (a new code perturbs nothing); sharing one is not.
///
/// Each variant has a fixed numeric [`RngDomain::code`]. **Changing a code changes every
/// derived key and therefore every golden digest in the repository**; add new variants
/// with new codes instead of renumbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RngDomain {
    /// Demand and spawn decisions (trip generation, class sampling, equipment fraction).
    Spawn,
    /// Car-following acceleration noise — and nothing else. Desired speeds come from
    /// [`RngDomain::DesiredSpeed`] and reaction times from [`RngDomain::ReactionTime`],
    /// so that a car-following model and a driver-heterogeneity model can draw for the
    /// same actor without interleaving.
    Mobility,
    /// Lane-change decisions.
    LaneChange,
    /// GNSS position/time error, outages, multipath.
    Gnss,
    /// Large-scale shadowing (log-normal, spatially correlated).
    Shadow,
    /// Small-scale fading (Rayleigh, Rice, Nakagami).
    Fading,
    /// MAC backoff (CSMA/CA contention window draws).
    MacBackoff,
    /// Sidelink semi-persistent scheduling resource selection.
    SpsSelection,
    /// Abstract-tier reception: the coin flip that replaces a modelled PHY.
    AbstractRx,
    /// Attacker behaviour (target choice, jitter, falsification magnitudes).
    Attack,
    /// Service-time sampling in queueing models (HSM, backend services).
    ServiceTime,
    /// Backend entity behaviour (batching jitter, retry timing).
    Backend,
    /// Collusion and Sybil group formation among attackers.
    Collusion,
    /// Perception noise (detection probability, bounding-box error, false positives).
    Perception,
    /// Misbehaviour report generation and sampling.
    Report,
    /// Modelled cryptography (pseudonym choice, nonce and digest material in
    /// `crypto_mode: modeled`; never used for real keys in `crypto_mode: real`).
    Crypto,
    /// Desired-speed sampling (driver heterogeneity), separate from
    /// [`RngDomain::Mobility`] so a car-following model and a heterogeneity model can
    /// both draw for one actor.
    DesiredSpeed,
    /// Driver reaction-time sampling, separate for the same reason.
    ReactionTime,
    /// An out-of-tree plug-in's own domain, derived from its model id
    /// ([`PluginDomain::for_model`]) and recorded in its model card's
    /// `determinism.rng_domains`.
    Plugin(PluginDomain),
}

impl RngDomain {
    /// The stable numeric code mixed into the stream key.
    ///
    /// Built-in domains occupy `1..=18`; a [`RngDomain::Plugin`] code always has its high
    /// bit set, so a plug-in's domain can never collide with a built-in one. Code `0` is
    /// reserved and unused.
    ///
    /// **Stability warning:** these numbers are part of the determinism contract. A
    /// changed code changes every key derived from it, which changes every draw, which
    /// changes every golden digest.
    pub const fn code(&self) -> u32 {
        match self {
            RngDomain::Spawn => 1,
            RngDomain::Mobility => 2,
            RngDomain::LaneChange => 3,
            RngDomain::Gnss => 4,
            RngDomain::Shadow => 5,
            RngDomain::Fading => 6,
            RngDomain::MacBackoff => 7,
            RngDomain::SpsSelection => 8,
            RngDomain::AbstractRx => 9,
            RngDomain::Attack => 10,
            RngDomain::ServiceTime => 11,
            RngDomain::Backend => 12,
            RngDomain::Collusion => 13,
            RngDomain::Perception => 14,
            RngDomain::Report => 15,
            RngDomain::Crypto => 16,
            RngDomain::DesiredSpeed => 17,
            RngDomain::ReactionTime => 18,
            RngDomain::Plugin(p) => p.0,
        }
    }

    /// The domain of the out-of-tree plug-in whose model card id is `model_id`.
    ///
    /// Shorthand for `RngDomain::Plugin(PluginDomain::for_model(id))`; see
    /// [`PluginDomain`] for why the code is derived rather than chosen.
    pub fn plugin(model_id: &str) -> Self {
        RngDomain::Plugin(PluginDomain::for_model(model_id))
    }

    /// The domain's kebab-case name, identical to its serde representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            RngDomain::Spawn => "spawn",
            RngDomain::Mobility => "mobility",
            RngDomain::LaneChange => "lane-change",
            RngDomain::Gnss => "gnss",
            RngDomain::Shadow => "shadow",
            RngDomain::Fading => "fading",
            RngDomain::MacBackoff => "mac-backoff",
            RngDomain::SpsSelection => "sps-selection",
            RngDomain::AbstractRx => "abstract-rx",
            RngDomain::Attack => "attack",
            RngDomain::ServiceTime => "service-time",
            RngDomain::Backend => "backend",
            RngDomain::Collusion => "collusion",
            RngDomain::Perception => "perception",
            RngDomain::Report => "report",
            RngDomain::Crypto => "crypto",
            RngDomain::DesiredSpeed => "desired-speed",
            RngDomain::ReactionTime => "reaction-time",
            RngDomain::Plugin(_) => "plugin",
        }
    }
}

impl core::fmt::Display for RngDomain {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RngDomain::Plugin(p) => write!(f, "{p}"),
            domain => f.write_str(domain.as_str()),
        }
    }
}

/// Who is drawing: the entity whose stream this is.
///
/// The variants cover the four scopes of 02-architecture.md §6.2 — actor, node, link and
/// `(link, frame)` — plus the world-global scope and a couple of convenience scopes for
/// static geometry. Encoding is explicit ([`EntityRef::encode`]) rather than derived from
/// Rust's `Hash`, whose output is neither stable across versions nor specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EntityRef {
    /// The run as a whole (scenario-level draws such as the weather timeline).
    Global,
    /// A physical actor.
    Actor(ActorId),
    /// A communicating node.
    Node(NodeId),
    /// A directed radio link `(tx, rx)`.
    Link(LinkKey),
    /// A single frame on a directed link: the scope of a small-scale fading draw, so
    /// that the draw does not depend on how many other frames the link carried.
    ///
    /// **Single-use scope** ([`EntityRef::is_single_use`]). The key already embeds the
    /// frame counter, so every draw has a fresh key and the stream is used exactly once.
    /// It is therefore never cached: [`RngRegistry::checkout`] derives, uses and drops it,
    /// and [`RngRegistry::stream`] refuses it. Caching it would intern one generator per
    /// frame per directed link and never free one.
    LinkFrame {
        /// The directed link.
        link: LinkKey,
        /// A per-link frame counter or a frame id.
        frame: u64,
    },
    /// A lane (spawn positions, per-lane noise).
    Lane(LaneId),
    /// A traffic signal (offset jitter in actuated plans).
    Signal(SignalId),
    /// A plug-in's own scope: `kind` discriminates the scope, `id` identifies the entity
    /// within it.
    ///
    /// Build it with [`EntityRef::custom`], which derives `kind` from the plug-in's model
    /// id, rather than by hand-picking a number that another plug-in may also pick.
    /// Treated as a long-lived scope, so the stream is cached and advances across draws; a
    /// plug-in that wants a per-frame scope uses [`EntityRef::LinkFrame`] or draws from an
    /// [`RngRegistry::ephemeral`] stream it owns for that one use.
    Custom {
        /// Scope discriminator, from [`EntityRef::custom`].
        kind: u16,
        /// Entity id within that scope.
        id: u64,
    },
}

impl EntityRef {
    /// A plug-in scope whose `kind` is derived from the plug-in's model card id.
    ///
    /// `kind = LE_u16(SHA-256(model_id)[0..2])`. Two plug-ins share a scope only on a
    /// 16-bit hash collision of their ids, which for the tens of plug-ins a run loads is
    /// a far smaller risk than two authors both writing `kind: 1`; the encoding keeps
    /// `kind` 16 bits because its byte layout is part of the determinism contract.
    pub fn custom(model_id: &str, id: u64) -> Self {
        let h = sha256(model_id.as_bytes());
        EntityRef::Custom {
            kind: u16::from_le_bytes([h[0], h[1]]),
            id,
        }
    }

    /// True if this scope's key already embeds a per-use counter, so its stream is drawn
    /// from exactly once and must not be cached.
    ///
    /// [`EntityRef::LinkFrame`] is the only such scope today. The distinction is what
    /// keeps the registry's cache bounded by the number of live entities rather than by
    /// the number of frames a run carries.
    pub const fn is_single_use(&self) -> bool {
        matches!(self, EntityRef::LinkFrame { .. })
    }

    /// Appends this entity's canonical byte encoding to `out`.
    ///
    /// The encoding is a one-byte tag followed by little-endian fields:
    ///
    /// | Tag | Variant | Payload |
    /// |---|---|---|
    /// | `0x00` | [`EntityRef::Global`] | — |
    /// | `0x01` | [`EntityRef::Actor`] | `u32` actor index |
    /// | `0x02` | [`EntityRef::Node`] | `u32` node index |
    /// | `0x03` | [`EntityRef::Link`] | `u32` tx, `u32` rx |
    /// | `0x04` | [`EntityRef::LinkFrame`] | `u32` tx, `u32` rx, `u64` frame |
    /// | `0x05` | [`EntityRef::Lane`] | `u32` lane index |
    /// | `0x06` | [`EntityRef::Signal`] | `u32` signal index |
    /// | `0x07` | [`EntityRef::Custom`] | `u16` kind, `u64` id |
    ///
    /// **Stability warning:** the tags and the field order are part of the determinism
    /// contract, exactly like [`RngDomain::code`].
    pub fn encode(&self, out: &mut Vec<u8>) {
        match *self {
            EntityRef::Global => out.push(0x00),
            EntityRef::Actor(a) => {
                out.push(0x01);
                out.extend_from_slice(&a.index().to_le_bytes());
            }
            EntityRef::Node(n) => {
                out.push(0x02);
                out.extend_from_slice(&n.index().to_le_bytes());
            }
            EntityRef::Link(l) => {
                out.push(0x03);
                out.extend_from_slice(&l.tx().index().to_le_bytes());
                out.extend_from_slice(&l.rx().index().to_le_bytes());
            }
            EntityRef::LinkFrame { link, frame } => {
                out.push(0x04);
                out.extend_from_slice(&link.tx().index().to_le_bytes());
                out.extend_from_slice(&link.rx().index().to_le_bytes());
                out.extend_from_slice(&frame.to_le_bytes());
            }
            EntityRef::Lane(l) => {
                out.push(0x05);
                out.extend_from_slice(&l.index().to_le_bytes());
            }
            EntityRef::Signal(s) => {
                out.push(0x06);
                out.extend_from_slice(&s.index().to_le_bytes());
            }
            EntityRef::Custom { kind, id } => {
                out.push(0x07);
                out.extend_from_slice(&kind.to_le_bytes());
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
    }

    /// This entity's canonical byte encoding as a fresh vector.
    pub fn encoding(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(16);
        self.encode(&mut v);
        v
    }
}

/// 2⁻⁵³: the scale that turns 53 random bits into a `f64` in `[0, 1)`. Exact.
const TWO_POW_MINUS_53: f64 = 1.0 / 9_007_199_254_740_992.0;

/// One deterministic stream: a ChaCha12 keystream plus this crate's samplers.
///
/// Streams are cheap to derive (one SHA-256) and cheap to keep: the [`RngRegistry`]
/// caches them so a stream's position advances across calls rather than restarting.
#[derive(Debug, Clone)]
pub struct RngStream {
    inner: ChaCha12Rng,
}

impl RngStream {
    /// Derives the stream for `(master_seed, domain, entity)`.
    ///
    /// `key = SHA-256(master_seed_le(8) ‖ domain.code()_le(4) ‖ entity.encode())`, and the
    /// stream is ChaCha12 seeded with that key **at word position zero**.
    ///
    /// # This is not how a model gets a stream
    ///
    /// Every call restarts the keystream at its key, so calling it twice for one key
    /// returns the same numbers twice. A model that reached for this instead of
    /// [`RngRegistry::checkout`] — for instance because the registry's `&mut self`
    /// accessor would not compile inside a parallel map — would silently draw a different
    /// sequence from the one the cached path produces, and the determinism contract
    /// ("identical, single- or multi-threaded") would be void with nothing detecting it.
    /// Use it only to reconstruct a stream whose position is being restored explicitly
    /// (snapshot replay, [`RngStream::set_word_pos`]) or to hand an out-of-process plug-in
    /// the one stream it owns for a batch (03-interfaces.md §16).
    pub fn derive(master_seed: u64, domain: RngDomain, entity: EntityRef) -> Self {
        let mut material = Vec::with_capacity(8 + 4 + 16);
        material.extend_from_slice(&master_seed.to_le_bytes());
        material.extend_from_slice(&domain.code().to_le_bytes());
        entity.encode(&mut material);
        Self::from_key(sha256(&material))
    }

    /// Builds a stream directly from a 32-byte key. Escape hatch for tests and for
    /// replay of a recorded key; production code uses [`RngStream::derive`].
    pub fn from_key(key: [u8; 32]) -> Self {
        Self {
            inner: ChaCha12Rng::from_seed(key),
        }
    }

    /// The next 64 random bits.
    pub fn u64(&mut self) -> u64 {
        self.inner.next_u64()
    }

    /// The next 32 random bits.
    ///
    /// Consumes one 32-bit word of the keystream, not a whole 64-bit one, so `u32()`
    /// twice and `u64()` once are *not* interchangeable.
    pub fn u32(&mut self) -> u32 {
        self.inner.next_u32()
    }

    /// Fills `dest` with keystream bytes.
    pub fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.inner.fill_bytes(dest);
    }

    /// A uniform `f64` in `[0, 1)`.
    ///
    /// `(u64 >> 11) × 2⁻⁵³`: the top 53 bits of one 64-bit draw, scaled by an exact
    /// power of two, so the conversion is exact on every platform
    /// (02-architecture.md §6.2). Consumes one 64-bit draw.
    pub fn f64(&mut self) -> f64 {
        ((self.u64() >> 11) as f64) * TWO_POW_MINUS_53
    }

    /// A Bernoulli trial: true with probability `p`.
    ///
    /// Consumes exactly one draw whatever `p` is — including `p ≤ 0` (always false, since
    /// [`RngStream::f64`] is never negative) and `p ≥ 1` (always true, since it is never
    /// `1.0`) — so a model's draw count never depends on a probability's value.
    pub fn bool(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    /// A uniform `f64` in `[a, b)` (or `(b, a]` if `b < a`). Consumes one draw.
    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f64()
    }

    /// A uniform integer in `[0, n)`, by Lemire's multiply-shift *without* rejection.
    ///
    /// Consumes exactly one 64-bit draw. The modulo bias is below 2⁻⁶⁴ ⋅ n and is
    /// accepted deliberately: a rejection loop would make the draw count depend on the
    /// keystream, which complicates reasoning about reproducibility for the sake of a
    /// bias no simulation can observe.
    ///
    /// # Panics
    /// If `n == 0`.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "RngStream::below(0) has no value to return");
        ((self.u64() as u128 * n as u128) >> 64) as u64
    }

    /// A normal (Gaussian) draw with mean `mu` and standard deviation `sigma`.
    ///
    /// Box–Muller with a **fixed branch**: `z = sqrt(-2 ln u₁) · cos(2π u₂)` with
    /// `u₁ = 1 − f64() ∈ (0, 1]` and `u₂ = f64()`. The second variate
    /// (`sin` instead of `cos`) is computed by nobody and cached by nobody, so a normal
    /// draw always consumes exactly two 64-bit draws regardless of call history
    /// (02-architecture.md §6.2). That costs one wasted variate and buys a draw count
    /// that never depends on how many normals were drawn before.
    pub fn normal(&mut self, mu: f64, sigma: f64) -> f64 {
        let u1 = 1.0 - self.f64(); // (0, 1]: never 0, so ln is finite.
        let u2 = self.f64();
        let r = math::sqrt(-2.0 * math::ln(u1));
        let theta = 2.0 * core::f64::consts::PI * u2;
        mu + sigma * r * math::cos(theta)
    }

    /// An exponential draw with rate `rate` (mean `1 / rate`).
    ///
    /// Inversion: `−ln(1 − U) / rate` with `U ∈ [0, 1)`, so `1 − U ∈ (0, 1]` and the
    /// result is finite and non-negative. Consumes one draw.
    ///
    /// # Panics
    /// If `rate` is not strictly positive and finite.
    pub fn exponential(&mut self, rate: f64) -> f64 {
        assert!(
            rate > 0.0 && rate.is_finite(),
            "exponential rate must be strictly positive and finite, got {rate}"
        );
        -math::ln(1.0 - self.f64()) / rate
    }

    /// A log-normal draw whose **logarithm** has mean `mu` and standard deviation
    /// `sigma`: `exp(normal(mu, sigma))`. Consumes two draws.
    ///
    /// This is the parameterisation the shadowing models use, where `sigma` is quoted in
    /// dB and the conversion to linear happens in the model, not here.
    pub fn lognormal(&mut self, mu: f64, sigma: f64) -> f64 {
        math::exp(self.normal(mu, sigma))
    }

    /// A gamma draw with shape `shape` (`k`) and scale `scale` (`θ`), mean `k·θ`.
    ///
    /// Marsaglia–Tsang (2000), "A simple method for generating gamma variables":
    ///
    /// * For `k ≥ 1`: with `d = k − 1/3` and `c = 1/sqrt(9d)`, repeat — draw `x ~ N(0,1)`,
    ///   set `v = (1 + c·x)³`, reject if `v ≤ 0`; draw `u ~ U[0,1)`; accept `d·v` if
    ///   `u < 1 − 0.0331·x⁴` (the cheap squeeze) or if
    ///   `ln u < x²/2 + d·(1 − v + ln v)` (the exact test).
    /// * For `k < 1`: boost with `G(k) = G(k+1) · U^(1/k)`, using `U = 1 − f64() ∈ (0, 1]`
    ///   so the power is finite.
    ///
    /// Being a rejection method, the draw count is not fixed; it is nonetheless fully
    /// determined by the stream's position, which is what determinism requires.
    /// Acceptance probability is above 0.98 for every shape.
    ///
    /// # Panics
    /// If `shape` or `scale` is not strictly positive and finite.
    pub fn gamma(&mut self, shape: f64, scale: f64) -> f64 {
        assert!(
            shape > 0.0 && shape.is_finite(),
            "gamma shape must be strictly positive and finite, got {shape}"
        );
        assert!(
            scale > 0.0 && scale.is_finite(),
            "gamma scale must be strictly positive and finite, got {scale}"
        );
        if shape < 1.0 {
            let g = self.gamma_standard(shape + 1.0);
            let u = 1.0 - self.f64(); // (0, 1]
            return g * math::pow(u, 1.0 / shape) * scale;
        }
        self.gamma_standard(shape) * scale
    }

    /// Marsaglia–Tsang for `shape ≥ 1` with unit scale.
    fn gamma_standard(&mut self, shape: f64) -> f64 {
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / math::sqrt(9.0 * d);
        loop {
            let x = self.normal(0.0, 1.0);
            let v = 1.0 + c * x;
            if v <= 0.0 {
                continue;
            }
            let v3 = v * v * v;
            let u = self.f64();
            let x2 = x * x;
            if u < 1.0 - 0.033_1 * x2 * x2 {
                return d * v3;
            }
            if math::ln(u) < 0.5 * x2 + d * (1.0 - v3 + math::ln(v3)) {
                return d * v3;
            }
        }
    }

    /// A Nakagami-*m* **amplitude** draw with shape `m` and spread `omega` (mean square).
    ///
    /// The Nakagami amplitude is the square root of a gamma-distributed power:
    /// `P ~ Gamma(shape = m, scale = Ω/m)` and `A = sqrt(P)`, so `E[A²] = Ω`. `m = 1`
    /// gives Rayleigh fading; larger `m` gives a more deterministic channel
    /// (04-models, radio fading family).
    ///
    /// # Panics
    /// If `m < 0.5` (outside the distribution's definition) or if `omega` is not
    /// strictly positive and finite.
    pub fn nakagami(&mut self, m: f64, omega: f64) -> f64 {
        assert!(
            m >= 0.5 && m.is_finite(),
            "Nakagami m must be at least 0.5 and finite, got {m}"
        );
        assert!(
            omega > 0.0 && omega.is_finite(),
            "Nakagami omega must be strictly positive and finite, got {omega}"
        );
        math::sqrt(self.gamma(m, omega / m))
    }

    /// Picks an index in `0..weights.len()` with probability proportional to its weight.
    ///
    /// One draw, one linear scan over the **caller's order**: the caller is responsible
    /// for passing weights in a deterministic (normally id-sorted) order, because
    /// floating-point summation is not commutative (02-architecture.md §6.3). If
    /// rounding lets the scan run off the end, the last positive weight wins.
    ///
    /// # Panics
    /// If `weights` is empty, contains a negative or non-finite weight, or sums to zero.
    pub fn choose_index(&mut self, weights: &[f64]) -> usize {
        assert!(
            !weights.is_empty(),
            "choose_index needs at least one weight"
        );
        let mut total = 0.0;
        for (i, w) in weights.iter().enumerate() {
            assert!(
                w.is_finite() && *w >= 0.0,
                "weight {i} must be finite and non-negative, got {w}"
            );
            total += *w;
        }
        assert!(total > 0.0, "choose_index needs a strictly positive total");
        let target = self.f64() * total;
        let mut acc = 0.0;
        let mut last_positive = 0;
        for (i, w) in weights.iter().enumerate() {
            if *w > 0.0 {
                last_positive = i;
                acc += *w;
                if target < acc {
                    return i;
                }
            }
        }
        last_positive
    }

    /// The stream's position, in 32-bit words, for snapshots and replay.
    pub fn word_pos(&self) -> u128 {
        self.inner.get_word_pos()
    }

    /// Rewinds or fast-forwards the stream to a word position from [`RngStream::word_pos`].
    pub fn set_word_pos(&mut self, pos: u128) {
        self.inner.set_word_pos(pos);
    }
}

/// A stream key: what a draw is for, and who it is for.
type Key = (RngDomain, EntityRef);

/// One shard of the stream cache. `None` marks a key whose stream is currently checked
/// out by an [`RngGuard`], which is how a second, concurrent checkout of the same key is
/// caught instead of being served a stream restarted at word 0.
type Shard = BTreeMap<Key, Option<RngStream>>;

/// Number of independently locked cache shards.
///
/// Only lock contention depends on this number; no drawn value does. A phase-parallel map
/// over 10,000 nodes on 8 threads touches 8 shards at a time out of 64, so a checkout
/// almost never waits, and the lock is held only for the `BTreeMap` lookup — never while
/// a model computes.
const SHARD_COUNT: usize = 64;

/// The shard a key lives in. Affects locking only, never values.
fn shard_of(domain: RngDomain, entity: EntityRef) -> usize {
    let (a, b) = match entity {
        EntityRef::Global => (0, 0),
        EntityRef::Actor(x) => (1, x.index() as u64),
        EntityRef::Node(x) => (2, x.index() as u64),
        EntityRef::Link(l) => (3, link_word(l)),
        EntityRef::LinkFrame { link, frame } => (4 ^ link_word(link), frame),
        EntityRef::Lane(x) => (5, x.index() as u64),
        EntityRef::Signal(x) => (6, x.index() as u64),
        EntityRef::Custom { kind, id } => (7 ^ kind as u64, id),
    };
    // FNV-1a over the three numeric parts of the key.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in [domain.code() as u64, a, b] {
        h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h >> 29) as usize % SHARD_COUNT
}

/// Packs a directed link into one word for hashing.
fn link_word(l: LinkKey) -> u64 {
    ((l.tx().index() as u64) << 32) | l.rx().index() as u64
}

/// Takes a shard's lock, ignoring poisoning.
///
/// A poisoned shard means a thread panicked while holding it, which in this engine means
/// the run is already being torn down; recovering the map is strictly better than a
/// second panic on the way out.
fn lock(shard: &Mutex<Shard>) -> MutexGuard<'_, Shard> {
    shard.lock().unwrap_or_else(|e| e.into_inner())
}

/// The set of live streams for a run: the master seed plus a sharded cache keyed by
/// `(domain, entity)`.
///
/// Caching is what makes a stream *a stream*: the second checkout of the same key
/// continues where the first left off instead of restarting from the key. Each shard is a
/// `BTreeMap` behind its own lock, so
///
/// * [`RngRegistry::checkout`] takes `&self` and works inside a `rayon` map over actors,
///   receivers or nodes (02-architecture.md §6.4) — which is where draws actually happen;
/// * [`RngRegistry::stream`] takes `&mut self` and skips the lock entirely, for the
///   single-threaded event loop;
/// * iteration and memory layout stay deterministic (nothing in the engine iterates the
///   cache today, but a snapshot exporter will).
///
/// Single-use scopes ([`EntityRef::is_single_use`]) are never cached, so the cache is
/// bounded by the number of live entities, not by the number of frames a run carries.
pub struct RngRegistry {
    master_seed: u64,
    shards: Box<[Mutex<Shard>]>,
}

impl RngRegistry {
    /// Creates a registry for a run's master seed.
    pub fn new(master_seed: u64) -> Self {
        Self {
            master_seed,
            shards: (0..SHARD_COUNT)
                .map(|_| Mutex::new(Shard::new()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    /// The run's master seed (recorded in the manifest, 02-architecture.md §6.5).
    pub fn master_seed(&self) -> u64 {
        self.master_seed
    }

    /// The stream for `(domain, entity)`, derived on first use and cached afterwards.
    ///
    /// **This is the one sanctioned way to draw**, and the implementation of
    /// [`crate::ctx::Ctx::rng`] (03-interfaces.md §1.1). It takes `&self`, so it can be
    /// called from inside a phase-parallel map; the returned [`RngGuard`] derefs to the
    /// stream and returns it to the cache when dropped, including on unwind.
    ///
    /// Single-use scopes ([`EntityRef::is_single_use`], i.e. [`EntityRef::LinkFrame`]) are
    /// served from a fresh derivation and dropped rather than cached — their key already
    /// embeds a per-frame counter, so caching them would grow without bound.
    ///
    /// # Panics
    /// If the same key is already checked out, whether by another thread or by a guard
    /// this thread is still holding. Two tasks drawing from one key would interleave in
    /// thread-scheduling order, which is exactly the nondeterminism this module exists to
    /// prevent, so it is a panic and not a silent re-derivation.
    pub fn checkout(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        if entity.is_single_use() {
            return RngGuard {
                home: None,
                key: (domain, entity),
                stream: Some(RngStream::derive(self.master_seed, domain, entity)),
            };
        }
        let key = (domain, entity);
        let mut shard = lock(&self.shards[shard_of(domain, entity)]);
        let stream = match shard.get_mut(&key) {
            Some(slot) => slot.take().unwrap_or_else(|| {
                panic!(
                    "RNG stream ({domain}, {entity:?}) is already checked out: exactly one \
                     task may draw from a key at a time"
                )
            }),
            None => {
                shard.insert(key, None);
                RngStream::derive(self.master_seed, domain, entity)
            }
        };
        drop(shard);
        RngGuard {
            home: Some(self),
            key,
            stream: Some(stream),
        }
    }

    /// The cached stream for `(domain, entity)` through a unique borrow of the registry.
    ///
    /// Same cache and same values as [`RngRegistry::checkout`], without the lock: for the
    /// single-threaded event loop, where `&mut self` is available. Inside a parallel phase
    /// use [`RngRegistry::checkout`].
    ///
    /// # Panics
    /// If `entity` is a single-use scope ([`EntityRef::is_single_use`]) — this accessor
    /// returns a reference into the cache and so has nowhere to put a stream that must not
    /// be cached — or if the key is currently checked out by a guard.
    pub fn stream(&mut self, domain: RngDomain, entity: EntityRef) -> &mut RngStream {
        assert!(
            !entity.is_single_use(),
            "{entity:?} is a single-use scope; call RngRegistry::checkout, which derives \
             and drops its stream instead of caching one per frame"
        );
        let seed = self.master_seed;
        let index = shard_of(domain, entity);
        let shard = self.shards[index]
            .get_mut()
            .unwrap_or_else(|e| e.into_inner());
        shard
            .entry((domain, entity))
            .or_insert_with(|| Some(RngStream::derive(seed, domain, entity)))
            .as_mut()
            .unwrap_or_else(|| {
                panic!("RNG stream ({domain}, {entity:?}) is checked out by a live RngGuard")
            })
    }

    /// A stream for `(domain, entity)` that is **not** cached and starts at word 0.
    ///
    /// The building block behind [`RngRegistry::checkout`]'s single-use path, exposed for
    /// the two callers that legitimately own a whole stream for one use: a single-use scope
    /// drawn outside the registry, and the seed handed to an out-of-process plug-in for a
    /// batch (03-interfaces.md §16). Calling it twice for one key yields the same numbers
    /// twice — see [`RngStream::derive`].
    pub fn ephemeral(&self, domain: RngDomain, entity: EntityRef) -> RngStream {
        RngStream::derive(self.master_seed, domain, entity)
    }

    /// True if `(domain, entity)` has been used already, including while checked out.
    ///
    /// Always false for a single-use scope, which is never cached.
    pub fn contains(&self, domain: RngDomain, entity: EntityRef) -> bool {
        lock(&self.shards[shard_of(domain, entity)]).contains_key(&(domain, entity))
    }

    /// Number of cached streams, including those currently checked out.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| lock(s).len()).sum()
    }

    /// True if no stream has been used yet.
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| lock(s).is_empty())
    }

    /// Drops every cached stream, so the next use of a key restarts it from its key.
    ///
    /// Intended for teardown and for tests. **Not for mid-run use:** rewinding a surviving
    /// entity's stream replays numbers it has already drawn. Mid-run reclamation of a
    /// despawned entity is [`RngRegistry::forget`], whose key is never used again because
    /// ids are not reused within a run (03-interfaces.md §1).
    pub fn clear(&mut self) {
        for shard in self.shards.iter_mut() {
            shard.get_mut().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    /// Drops the cached stream for `(domain, entity)`; returns whether one was cached.
    ///
    /// The despawn path: an id is never reused within a run, so forgetting a despawned
    /// entity's streams cannot rewind anything that still draws.
    pub fn forget(&mut self, domain: RngDomain, entity: EntityRef) -> bool {
        let index = shard_of(domain, entity);
        self.shards[index]
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(domain, entity))
            .is_some()
    }

    /// Puts a checked-out stream back into its shard.
    fn restore(&self, key: Key, stream: RngStream) {
        lock(&self.shards[shard_of(key.0, key.1)]).insert(key, Some(stream));
    }
}

impl Clone for RngRegistry {
    fn clone(&self) -> Self {
        Self {
            master_seed: self.master_seed,
            shards: self
                .shards
                .iter()
                .map(|s| Mutex::new(lock(s).clone()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

impl core::fmt::Debug for RngRegistry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RngRegistry")
            .field("master_seed", &self.master_seed)
            .field("streams", &self.len())
            .finish()
    }
}

/// A stream borrowed out of an [`RngRegistry`], returned to it on drop.
///
/// Derefs to [`RngStream`], so `reg.checkout(d, e).normal(0.0, 1.0)` reads like a method
/// call on the stream. Holding the guard across a call that needs `&mut` access to the
/// engine context will not compile; the usual shape is one statement per draw batch:
///
/// ```
/// # use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
/// # use v2xw_core::ids::ActorId;
/// let reg = RngRegistry::new(42);
/// let noise = {
///     let mut rng = reg.checkout(RngDomain::Mobility, EntityRef::Actor(ActorId::new(7)));
///     rng.normal(0.0, 0.2)
/// };
/// # let _ = noise;
/// ```
///
/// A guard for a single-use scope owns its stream outright and discards it on drop; for
/// every other scope it holds the cache's slot open, so a second checkout of the same key
/// panics rather than starting a second copy of the keystream.
#[derive(Debug)]
pub struct RngGuard<'a> {
    /// The registry to return the stream to, or `None` for a single-use scope.
    home: Option<&'a RngRegistry>,
    key: Key,
    /// Always `Some` until [`Drop`] takes it.
    stream: Option<RngStream>,
}

impl RngGuard<'_> {
    /// The key this guard was checked out for.
    pub fn key(&self) -> (RngDomain, EntityRef) {
        self.key
    }
}

impl core::ops::Deref for RngGuard<'_> {
    type Target = RngStream;
    fn deref(&self) -> &RngStream {
        self.stream.as_ref().expect("stream is taken only on drop")
    }
}

impl core::ops::DerefMut for RngGuard<'_> {
    fn deref_mut(&mut self) -> &mut RngStream {
        self.stream.as_mut().expect("stream is taken only on drop")
    }
}

impl Drop for RngGuard<'_> {
    fn drop(&mut self) {
        if let (Some(home), Some(stream)) = (self.home, self.stream.take()) {
            home.restore(self.key, stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden values pinned by `golden_stream_values`; see that test.
    const GOLDEN_KEY_HEX: &str = "a19775b89ef31f9d8a7fcaa4f052afee0b2a693c90764e9d7a845200ef714b06";
    const GOLDEN_U64: u64 = 0x6070_085f_857f_da14;
    const GOLDEN_F64_BITS: u64 = 0x3f9e_555e_8de0_fc40;

    fn actor(i: u32) -> EntityRef {
        EntityRef::Actor(ActorId::new(i))
    }

    #[test]
    fn domain_codes_are_stable() {
        // Pinning the numbers: a change here is a change to every golden digest.
        assert_eq!(RngDomain::Spawn.code(), 1);
        assert_eq!(RngDomain::Mobility.code(), 2);
        assert_eq!(RngDomain::LaneChange.code(), 3);
        assert_eq!(RngDomain::Gnss.code(), 4);
        assert_eq!(RngDomain::Shadow.code(), 5);
        assert_eq!(RngDomain::Fading.code(), 6);
        assert_eq!(RngDomain::MacBackoff.code(), 7);
        assert_eq!(RngDomain::SpsSelection.code(), 8);
        assert_eq!(RngDomain::AbstractRx.code(), 9);
        assert_eq!(RngDomain::Attack.code(), 10);
        assert_eq!(RngDomain::ServiceTime.code(), 11);
        assert_eq!(RngDomain::Backend.code(), 12);
        assert_eq!(RngDomain::Collusion.code(), 13);
        assert_eq!(RngDomain::Perception.code(), 14);
        assert_eq!(RngDomain::Report.code(), 15);
        assert_eq!(RngDomain::Crypto.code(), 16);
        assert_eq!(RngDomain::DesiredSpeed.code(), 17);
        assert_eq!(RngDomain::ReactionTime.code(), 18);
    }

    /// A plug-in's domain code is a function of its model id, so two plug-ins collide only
    /// if their ids collide — never because both authors picked the same number.
    #[test]
    fn plugin_domain_codes_come_from_the_model_id() {
        let a = RngDomain::plugin("radio/fading/my-model");
        let b = RngDomain::plugin("mobility/other-authors-model");
        assert_ne!(a.code(), b.code());
        assert_eq!(a.code(), RngDomain::plugin("radio/fading/my-model").code());
        // Pinned: this is part of the determinism contract for that id.
        assert_eq!(
            PluginDomain::for_model("radio/fading/my-model").code(),
            0xbfa1_0e86,
            "SHA-256(id)[0..4] = 0d 1d 42 7f, little-endian, shifted right one bit"
        );
        // The high bit is always set, so no built-in domain can collide.
        for d in [a, b, RngDomain::plugin("")] {
            assert_ne!(d.code() & 0x8000_0000, 0);
            assert!(d.code() > RngDomain::ReactionTime.code());
        }
        assert_eq!(a.as_str(), "plugin");
        assert_eq!(a.to_string(), format!("plugin-{:08x}", a.code()));
    }

    #[test]
    fn entity_encoding_is_stable_and_distinct() {
        assert_eq!(EntityRef::Global.encoding(), vec![0x00]);
        assert_eq!(actor(1).encoding(), vec![0x01, 1, 0, 0, 0]);
        assert_eq!(
            EntityRef::Node(NodeId::new(258)).encoding(),
            vec![0x02, 2, 1, 0, 0]
        );
        let link = LinkKey::new(NodeId::new(1), NodeId::new(2));
        assert_eq!(
            EntityRef::Link(link).encoding(),
            vec![0x03, 1, 0, 0, 0, 2, 0, 0, 0]
        );
        assert_eq!(
            EntityRef::LinkFrame { link, frame: 3 }.encoding(),
            vec![0x04, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(EntityRef::Lane(LaneId::new(9)).encoding()[0], 0x05);
        assert_eq!(EntityRef::Signal(SignalId::new(9)).encoding()[0], 0x06);
        assert_eq!(
            EntityRef::Custom { kind: 1, id: 2 }.encoding(),
            vec![0x07, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0]
        );
        // The link direction is part of the key.
        assert_ne!(
            EntityRef::Link(link).encoding(),
            EntityRef::Link(link.reversed()).encoding()
        );
    }

    #[test]
    fn same_seed_gives_identical_sequences() {
        let mut a = RngStream::derive(0xDEAD_BEEF, RngDomain::Fading, actor(7));
        let mut b = RngStream::derive(0xDEAD_BEEF, RngDomain::Fading, actor(7));
        for _ in 0..64 {
            assert_eq!(a.u64(), b.u64());
        }
    }

    #[test]
    fn different_keys_give_different_sequences() {
        let base = RngStream::derive(1, RngDomain::Fading, actor(1)).u64();
        let other_seed = RngStream::derive(2, RngDomain::Fading, actor(1)).u64();
        let other_domain = RngStream::derive(1, RngDomain::Shadow, actor(1)).u64();
        let other_entity = RngStream::derive(1, RngDomain::Fading, actor(2)).u64();
        let other_kind =
            RngStream::derive(1, RngDomain::Fading, EntityRef::Node(NodeId::new(1))).u64();
        assert_ne!(base, other_seed);
        assert_ne!(base, other_domain);
        assert_ne!(base, other_entity);
        assert_ne!(base, other_kind);
    }

    /// The key property of ADR 0004 §3: a stream's own sequence does not depend on how
    /// other streams are interleaved with it.
    #[test]
    fn streams_are_order_independent() {
        let mut reg = RngRegistry::new(42);
        let (d, e1, e2) = (RngDomain::Mobility, actor(1), actor(2));

        // Order A: all of stream 1, then all of stream 2.
        let mut seq1_a = Vec::new();
        let mut seq2_a = Vec::new();
        for _ in 0..16 {
            seq1_a.push(reg.stream(d, e1).u64());
        }
        for _ in 0..16 {
            seq2_a.push(reg.stream(d, e2).u64());
        }

        // Order B: interleaved, in a fresh registry with the same seed.
        let mut reg_b = RngRegistry::new(42);
        let mut seq1_b = Vec::new();
        let mut seq2_b = Vec::new();
        for _ in 0..16 {
            seq2_b.push(reg_b.stream(d, e2).u64());
            seq1_b.push(reg_b.stream(d, e1).u64());
        }

        assert_eq!(seq1_a, seq1_b);
        assert_eq!(seq2_a, seq2_b);

        // Order C: an unrelated third stream drawn in between must not perturb either.
        let mut reg_c = RngRegistry::new(42);
        let mut seq1_c = Vec::new();
        for i in 0..16 {
            let _ = reg_c.stream(RngDomain::Attack, actor(99)).u64();
            if i % 2 == 0 {
                let _ = reg_c.stream(d, e2).f64();
            }
            seq1_c.push(reg_c.stream(d, e1).u64());
        }
        assert_eq!(seq1_a, seq1_c);
    }

    #[test]
    fn registry_caches_and_advances_streams() {
        let mut reg = RngRegistry::new(7);
        assert!(reg.is_empty());
        let first = reg.stream(RngDomain::Spawn, EntityRef::Global).u64();
        let second = reg.stream(RngDomain::Spawn, EntityRef::Global).u64();
        assert_ne!(first, second, "the cached stream must advance, not restart");
        assert_eq!(reg.len(), 1);
        assert!(reg.contains(RngDomain::Spawn, EntityRef::Global));
        assert_eq!(reg.master_seed(), 7);

        // Forgetting restarts from the key.
        assert!(reg.forget(RngDomain::Spawn, EntityRef::Global));
        assert_eq!(reg.stream(RngDomain::Spawn, EntityRef::Global).u64(), first);
        reg.clear();
        assert!(reg.is_empty());
    }

    #[test]
    fn uniforms_are_in_range_and_use_53_bits() {
        let mut s = RngStream::derive(1, RngDomain::plugin("x/plug"), EntityRef::Global);
        for _ in 0..10_000 {
            let u = s.f64();
            assert!((0.0..1.0).contains(&u));
            let x = s.uniform(-3.0, 5.0);
            assert!((-3.0..5.0).contains(&x));
        }
        // The scale is exactly 2^-53.
        assert_eq!(TWO_POW_MINUS_53, math::pow(2.0, -53.0));
        assert_eq!(TWO_POW_MINUS_53 * 9_007_199_254_740_992.0, 1.0);
    }

    #[test]
    fn bernoulli_uses_one_draw_whatever_p_is() {
        let mut s = RngStream::derive(5, RngDomain::AbstractRx, EntityRef::Global);
        let start = s.word_pos();
        assert!(!s.bool(0.0));
        assert!(s.bool(1.0));
        let _ = s.bool(0.5);
        assert_eq!(
            s.word_pos(),
            start + 3 * 2,
            "each bool costs one u64 = 2 words"
        );

        // And it is calibrated.
        let mut s = RngStream::derive(6, RngDomain::AbstractRx, EntityRef::Global);
        let hits = (0..20_000).filter(|_| s.bool(0.25)).count();
        assert!((4_500..5_500).contains(&hits), "got {hits}");
    }

    #[test]
    fn below_is_in_range() {
        let mut s = RngStream::derive(11, RngDomain::MacBackoff, EntityRef::Global);
        let mut seen = [0usize; 8];
        for _ in 0..8_000 {
            let v = s.below(8);
            assert!(v < 8);
            seen[v as usize] += 1;
        }
        assert!(seen.iter().all(|c| *c > 800), "{seen:?}");
        assert_eq!(s.below(1), 0);
    }

    #[test]
    fn normal_is_calibrated_and_costs_two_draws() {
        let mut s = RngStream::derive(2, RngDomain::Shadow, EntityRef::Global);
        let start = s.word_pos();
        let _ = s.normal(0.0, 1.0);
        assert_eq!(s.word_pos(), start + 4, "two u64 draws = four 32-bit words");

        let n = 50_000;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for _ in 0..n {
            let z = s.normal(3.0, 2.0);
            sum += z;
            sum_sq += z * z;
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64 - mean * mean;
        assert!((mean - 3.0).abs() < 0.05, "mean {mean}");
        assert!((var - 4.0).abs() < 0.15, "var {var}");
    }

    #[test]
    fn exponential_is_calibrated() {
        let mut s = RngStream::derive(3, RngDomain::ServiceTime, EntityRef::Global);
        let n = 50_000;
        let mut sum = 0.0;
        for _ in 0..n {
            let x = s.exponential(4.0);
            assert!(x >= 0.0 && x.is_finite());
            sum += x;
        }
        let mean = sum / n as f64;
        assert!((mean - 0.25).abs() < 0.01, "mean {mean}");
    }

    #[test]
    fn lognormal_matches_its_definition() {
        let mut a = RngStream::derive(4, RngDomain::Shadow, actor(1));
        let mut b = a.clone();
        let x = a.lognormal(0.5, 1.5);
        let z = b.normal(0.5, 1.5);
        assert_eq!(x, math::exp(z));
        assert!(x > 0.0);
    }

    #[test]
    fn gamma_is_calibrated_for_both_branches() {
        for (shape, scale) in [(0.4_f64, 2.0_f64), (1.0, 1.0), (2.5, 0.5), (12.0, 3.0)] {
            let mut s = RngStream::derive(9, RngDomain::Fading, EntityRef::Global);
            let n = 40_000;
            let mut sum = 0.0;
            let mut sum_sq = 0.0;
            for _ in 0..n {
                let x = s.gamma(shape, scale);
                assert!(x > 0.0 && x.is_finite(), "gamma({shape}, {scale}) = {x}");
                sum += x;
                sum_sq += x * x;
            }
            let mean = sum / n as f64;
            let var = sum_sq / n as f64 - mean * mean;
            let want_mean = shape * scale;
            let want_var = shape * scale * scale;
            assert!(
                (mean - want_mean).abs() < 0.05 * want_mean.max(1.0),
                "shape {shape}: mean {mean} want {want_mean}"
            );
            assert!(
                (var - want_var).abs() < 0.12 * want_var.max(1.0),
                "shape {shape}: var {var} want {want_var}"
            );
        }
    }

    #[test]
    fn nakagami_has_the_right_mean_square() {
        for m in [0.5_f64, 1.0, 3.0] {
            let mut s = RngStream::derive(10, RngDomain::Fading, EntityRef::Global);
            let n = 40_000;
            let mut sum_sq = 0.0;
            for _ in 0..n {
                let a = s.nakagami(m, 2.0);
                assert!(a >= 0.0 && a.is_finite());
                sum_sq += a * a;
            }
            let mean_sq = sum_sq / n as f64;
            assert!((mean_sq - 2.0).abs() < 0.08, "m {m}: E[A^2] = {mean_sq}");
        }
    }

    #[test]
    fn choose_index_respects_weights() {
        let mut s = RngStream::derive(12, RngDomain::Spawn, EntityRef::Global);
        let weights = [1.0, 3.0, 0.0, 4.0];
        let mut counts = [0usize; 4];
        for _ in 0..40_000 {
            counts[s.choose_index(&weights)] += 1;
        }
        assert_eq!(counts[2], 0, "a zero weight must never be chosen");
        let total: usize = counts.iter().sum();
        let share = |i: usize| counts[i] as f64 / total as f64;
        assert!((share(0) - 0.125).abs() < 0.01, "{counts:?}");
        assert!((share(1) - 0.375).abs() < 0.01, "{counts:?}");
        assert!((share(3) - 0.500).abs() < 0.01, "{counts:?}");
        assert_eq!(s.choose_index(&[0.0, 5.0]), 1);
        assert_eq!(s.choose_index(&[2.0]), 0);
    }

    #[test]
    #[should_panic(expected = "strictly positive total")]
    fn choose_index_rejects_zero_total() {
        let mut s = RngStream::derive(13, RngDomain::Spawn, EntityRef::Global);
        let _ = s.choose_index(&[0.0, 0.0]);
    }

    #[test]
    #[should_panic(expected = "exponential rate")]
    fn exponential_rejects_zero_rate() {
        let mut s = RngStream::derive(14, RngDomain::ServiceTime, EntityRef::Global);
        let _ = s.exponential(0.0);
    }

    #[test]
    fn word_position_round_trips() {
        let mut s = RngStream::derive(15, RngDomain::Backend, EntityRef::Global);
        let pos = s.word_pos();
        let a = [s.u64(), s.u64(), s.u64()];
        s.set_word_pos(pos);
        let b = [s.u64(), s.u64(), s.u64()];
        assert_eq!(a, b);
    }

    /// Golden vector: pins the whole derivation (entity encoding, SHA-256 key, ChaCha12
    /// keystream, 53-bit float conversion) so that any accidental change to any of them
    /// fails here rather than silently invalidating every recorded dataset.
    #[test]
    fn golden_stream_values() {
        // Key material for (seed 0, Spawn, Global) spelled out by hand.
        let mut material = Vec::new();
        material.extend_from_slice(&0u64.to_le_bytes());
        material.extend_from_slice(&1u32.to_le_bytes()); // RngDomain::Spawn.code()
        material.push(0x00); // EntityRef::Global
        assert_eq!(
            crate::hash::sha256_hex(&material),
            GOLDEN_KEY_HEX,
            "stream key derivation changed"
        );

        let mut s = RngStream::derive(0, RngDomain::Spawn, EntityRef::Global);
        assert_eq!(s.u64(), GOLDEN_U64);
        assert_eq!(s.f64().to_bits(), GOLDEN_F64_BITS);

        let mut from_key = RngStream::from_key(crate::hash::sha256(&material));
        assert_eq!(from_key.u64(), GOLDEN_U64);
    }

    #[test]
    fn serde_round_trips_domains_and_entities() {
        let d = RngDomain::MacBackoff;
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(s, "\"mac-backoff\"");
        assert_eq!(serde_json::from_str::<RngDomain>(&s).unwrap(), d);
        // A plug-in domain round-trips by its derived code, so a recorded card or
        // manifest can be read back without the plug-in being present.
        let p = RngDomain::plugin("x/plug");
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, format!("{{\"plugin\":{}}}", p.code()));
        assert_eq!(serde_json::from_str::<RngDomain>(&s).unwrap(), p);
        let e = EntityRef::LinkFrame {
            link: LinkKey::new(NodeId::new(1), NodeId::new(2)),
            frame: 9,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<EntityRef>(&s).unwrap(), e);
        assert_eq!(RngDomain::MacBackoff.to_string(), "mac-backoff");
        assert_eq!(RngDomain::MacBackoff.as_str(), "mac-backoff");
        assert_eq!(RngDomain::DesiredSpeed.as_str(), "desired-speed");
    }

    /// A plug-in scope's discriminator comes from its model id, like its domain code.
    #[test]
    fn custom_scopes_are_derived_from_the_model_id() {
        let a = EntityRef::custom("x/plug", 7);
        assert_eq!(
            a,
            EntityRef::Custom {
                kind: 36_866,
                id: 7
            }
        );
        assert_ne!(a, EntityRef::custom("y/other-plug", 7));
        assert!(
            !a.is_single_use(),
            "a plug-in scope is cached like any other"
        );
    }

    /// The headline determinism claim: identical values single- or multi-threaded.
    ///
    /// The phase below is the shape of 02-architecture.md §6.4 — a pure map over actors,
    /// merged in id order — run through `rayon` with 1 and with 8 threads, and compared
    /// against the sequential event-loop accessor. All three must agree bit for bit,
    /// across several phases so that stream *continuation* is exercised, not just the
    /// first draw of each key.
    #[test]
    fn parallel_phase_is_thread_count_independent() {
        use rayon::prelude::*;

        const ACTORS: u32 = 256;
        const PHASES: usize = 4;

        fn phase(reg: &RngRegistry) -> Vec<u64> {
            (0..ACTORS)
                .into_par_iter()
                .map(|i| {
                    let e = EntityRef::Actor(ActorId::new(i));
                    let mut noise = reg.checkout(RngDomain::Mobility, e);
                    let a = noise.normal(0.0, 1.0).to_bits();
                    drop(noise);
                    // A second domain for the same actor, and a single-use link frame.
                    let b = reg.checkout(RngDomain::DesiredSpeed, e).f64().to_bits();
                    let link = LinkKey::new(NodeId::new(i), NodeId::new(i + 1));
                    let c = reg
                        .checkout(RngDomain::Fading, EntityRef::LinkFrame { link, frame: 3 })
                        .nakagami(1.0, 1.0)
                        .to_bits();
                    a ^ b.rotate_left(17) ^ c.rotate_left(33)
                })
                .collect() // `collect` over an indexed parallel iterator preserves id order
        }

        fn run_with(threads: usize) -> Vec<Vec<u64>> {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let reg = RngRegistry::new(0xFEED_FACE);
            pool.install(|| (0..PHASES).map(|_| phase(&reg)).collect())
        }

        let one = run_with(1);
        let eight = run_with(8);
        assert_eq!(one, eight, "thread count changed the drawn values");

        // …and the single-threaded event-loop accessor agrees with both.
        let mut reg = RngRegistry::new(0xFEED_FACE);
        let sequential: Vec<Vec<u64>> = (0..PHASES)
            .map(|_| {
                (0..ACTORS)
                    .map(|i| {
                        let e = EntityRef::Actor(ActorId::new(i));
                        let a = reg
                            .stream(RngDomain::Mobility, e)
                            .normal(0.0, 1.0)
                            .to_bits();
                        let b = reg.stream(RngDomain::DesiredSpeed, e).f64().to_bits();
                        let link = LinkKey::new(NodeId::new(i), NodeId::new(i + 1));
                        let c = reg
                            .checkout(RngDomain::Fading, EntityRef::LinkFrame { link, frame: 3 })
                            .nakagami(1.0, 1.0)
                            .to_bits();
                        a ^ b.rotate_left(17) ^ c.rotate_left(33)
                    })
                    .collect()
            })
            .collect();
        assert_eq!(one, sequential, "the &mut and &self accessors disagree");

        // The second phase differs from the first: cached streams advance, they do not
        // restart at their key.
        assert_ne!(one[0], one[1]);
    }

    /// Single-use scopes are not interned, so the cache stays bounded by the number of
    /// live entities rather than by the number of frames the run carries.
    #[test]
    fn single_use_scopes_are_never_cached() {
        let reg = RngRegistry::new(3);
        let link = LinkKey::new(NodeId::new(1), NodeId::new(2));
        let mut values = Vec::new();
        for frame in 0..1_000 {
            values.push(
                reg.checkout(RngDomain::Fading, EntityRef::LinkFrame { link, frame })
                    .f64(),
            );
        }
        assert!(
            reg.is_empty(),
            "1000 frame draws interned {} streams",
            reg.len()
        );
        assert!(!reg.contains(RngDomain::Fading, EntityRef::LinkFrame { link, frame: 0 }));
        // Each frame is its own key, so the values differ…
        assert_ne!(values[0], values[1]);
        // …and re-drawing a frame reproduces it exactly, which is the point of the scope.
        assert_eq!(
            reg.checkout(RngDomain::Fading, EntityRef::LinkFrame { link, frame: 0 })
                .f64(),
            values[0]
        );
        // A long-lived scope is cached, by contrast.
        let _ = reg.checkout(RngDomain::Fading, EntityRef::Link(link)).f64();
        assert_eq!(reg.len(), 1);
    }

    #[test]
    #[should_panic(expected = "already checked out")]
    fn two_live_guards_for_one_key_panic() {
        let reg = RngRegistry::new(1);
        let e = EntityRef::Actor(ActorId::new(1));
        let _first = reg.checkout(RngDomain::Mobility, e);
        let _second = reg.checkout(RngDomain::Mobility, e);
    }

    #[test]
    #[should_panic(expected = "single-use scope")]
    fn the_mut_accessor_refuses_single_use_scopes() {
        let mut reg = RngRegistry::new(1);
        let link = LinkKey::new(NodeId::new(1), NodeId::new(2));
        let _ = reg.stream(RngDomain::Fading, EntityRef::LinkFrame { link, frame: 1 });
    }

    /// A guard returns its stream even when the drawing code panics, so a caught panic
    /// cannot leave a key permanently unusable.
    #[test]
    fn a_guard_returns_its_stream_on_unwind() {
        let reg = RngRegistry::new(5);
        let e = EntityRef::Node(NodeId::new(4));
        let first = reg.checkout(RngDomain::Gnss, e).u64();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut g = reg.checkout(RngDomain::Gnss, e);
            let _ = g.u64();
            panic!("model failed mid-draw");
        }));
        assert!(caught.is_err());
        // The slot is usable again, and the stream kept the position it had reached.
        assert_ne!(reg.checkout(RngDomain::Gnss, e).u64(), first);
    }

    /// The registry is cloneable (snapshots) and its clone is independent.
    #[test]
    fn cloning_snapshots_the_streams() {
        let mut reg = RngRegistry::new(11);
        let e = EntityRef::Actor(ActorId::new(2));
        let _ = reg.stream(RngDomain::Spawn, e).u64();
        let snapshot = reg.clone();
        let live = reg.stream(RngDomain::Spawn, e).u64();
        assert_eq!(
            snapshot.checkout(RngDomain::Spawn, e).u64(),
            live,
            "a clone continues from the same position"
        );
        // …but advancing the clone does not advance the original.
        let _ = snapshot.checkout(RngDomain::Spawn, e).u64();
        assert_eq!(reg.len(), snapshot.len());
        assert_eq!(
            format!("{reg:?}"),
            "RngRegistry { master_seed: 11, streams: 1 }"
        );
    }
}
