//! `fragmenter/cert-cycle-partial-hybrid` — the NDSS 2024 Partially-Hybrid certificate
//! cycle (04-models.md §7.3).
//!
//! The scheme, in one paragraph. BSMs keep their ECDSA signatures, so verification cost and
//! message size are unchanged. What has to be distributed is a **hybrid certificate**: the
//! ECDSA certificate plus a post-quantum signature over the ECDSA key, of size
//! `30 + pk + sig`. It is far too large for one frame, so it is split into **α equal
//! fragments carried in the first α SPDUs of each certificate cycle** of τ = 5 SPDUs
//! (500 ms at the 10 Hz BSM rate); the remaining SPDUs of the cycle carry the certificate's
//! digest, exactly as the ordinary J2945/1 certificate-omission rule does.
//! P2PCD learning responses are split the same way, into β fragments each sent after a
//! uniform 0-250 ms wait.
//!
//! [Twardokus, Bindel, Rahbari and McCarthy, "When Cryptography Needs a Hand: Practical
//! Post-Quantum Authentication for V2V Communications", NDSS 2024, §II-B, §III, §IV, §V-D;
//! R4 §E; R5 §B.6. VERIFIED as a secondary source.]
//!
//! # The documented anchors
//!
//! | Quantity | Value | Where |
//! |---|---|---|
//! | Cycle length τ | 5 SPDUs = 500 ms | NDSS §IV |
//! | ECDSA certificate | 162 B | NDSS |
//! | Falcon-512 hybrid certificate | 858 B | NDSS Fig. 4 |
//! | α for Falcon-512 | 1 | NDSS |
//! | First frame of the cycle | 1,026 B | NDSS Fig. 4 |
//! | The other four frames | 204 B each | NDSS Fig. 4 |
//! | DSRC payload cap | 2,304 B | 802.11 Table 9-25 |
//! | C-V2X payload cap (10 MHz, practical MCS) | 437 B | 3GPP Table A.8.3-1 via NDSS |
//! | P2PCD response backoff | uniform 0-250 ms, mean 125 ms | IEEE 1609.2 §8, NDSS |
//! | Learning-response times | Falcon 250 ms, XMSS 375 ms, Dilithium ≈ 500 ms, SPHINCS+ ≈ 1,000 ms (8 fragments) | NDSS §V-D |
//! | Added per-BSM delay | 0.25-0.39 ms | NDSS §V-D |
//!
//! [`CertCyclePartialHybrid::alpha`] reproduces the α anchor: at the DSRC cap of 2,304 B
//! with the cycle's own 204 B frames, one fragment of 858 B fits, so α = 1 — and with α = 1
//! the certificate travels in a single frame, so `n = 1` and **the loss amplification of
//! §7.4 does not bite at all** for Falcon-512, which is the design's central claim.
//!
//! # Two readings of `30 + pk + sig`
//!
//! The certificate-size formula and the Falcon-512 anchor do not agree. With Falcon-512's
//! published key sizes (pk 897 B, sig 666 B) `30 + pk + sig` is 1,593 B, not the 858 B
//! Fig. 4 reports — and 858 is exactly `30 + 162 + 666`, the ECDSA certificate plus the
//! Falcon signature over it. So the formula's `pk` term reads more naturally as *the ECDSA
//! certificate the PQ signature covers* than as the PQ public key.
//!
//! Neither reading is invented here: [`CertCycleParams::hybrid_cert_bytes`] defaults to the
//! 858 B Fig. 4 anchor, the other reading is one parameter override away, and α is 1 at the
//! DSRC cap under both (1,593 B still fits a 2,100 B budget), so the design's conclusion
//! does not turn on which is right.
//!
//! # What this model does not reproduce, on purpose
//!
//! The NDSS frame totals (1,026 B and 204 B) are *whole frames*: MAC header, security
//! envelope, BSM and credential together. This model sizes **the credential fragments
//! only**; the BSM is `v2xw-msg`'s, the envelope and certificate are `v2xw-sec`'s and the
//! MAC overhead is `v2xw-radio`'s (04-models.md §4.6, §8.2, §9.1). Adding those totals here
//! would double-count bytes another crate already owns, which invariant I-N1 forbids, so the
//! frame anchors are carried as constants for cross-checking and are *not* returned as
//! sizes.

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::{Ctx, CtxExt};
use v2xw_core::ids::{NodeId, SduId};
use v2xw_core::model::Model;
use v2xw_core::registry::ParamSet;
use v2xw_core::time::{Duration, NS_PER_MS, SimTime};

use crate::amplification;
use crate::error::DropCause;
use crate::frag::{
    FragRecord, FragmentDesc, FragmentKind, Fragmenter, ReassemblyBuffer, ReassemblyOutcome,
    split_equal,
};

/// This model's stable id.
pub const FRAGMENTER_CERT_CYCLE_ID: &str = "fragmenter/cert-cycle-partial-hybrid";

/// τ: the certificate-transmission cycle, in SPDUs [NDSS 2024 §IV].
pub const CERT_CYCLE_SPDUS: u16 = 5;

/// The cycle's duration at the 10 Hz BSM rate, milliseconds: τ = 5 SPDUs = 500 ms
/// [NDSS 2024 §IV; J2945/1 certificate omission every fifth SPDU, R4 §E].
pub const CERT_CYCLE_PERIOD_MS: u64 = 500;

/// An ECDSA certificate, bytes [NDSS 2024, VERIFIED secondary].
pub const ECDSA_CERTIFICATE_BYTES: u32 = 162;

/// The Falcon-512 hybrid certificate, bytes [NDSS 2024 Fig. 4, VERIFIED secondary].
pub const FALCON512_HYBRID_CERTIFICATE_BYTES: u32 = 858;

/// A certificate digest (HashedId8), bytes [IEEE 1609.2 §6.3.25 via 04-models.md §9.1].
pub const CERT_DIGEST_BYTES: u32 = 8;

/// The DSRC payload cap, bytes [802.11 Table 9-25 via NDSS 2024; 04-models.md §4.6].
pub const DSRC_PAYLOAD_CAP_BYTES: u32 = 2_304;

/// The C-V2X payload cap at the practical MCS in 10 MHz, bytes
/// [3GPP Table A.8.3-1 via NDSS 2024 §V-D].
pub const CV2X_PAYLOAD_CAP_BYTES: u32 = 437;

/// The digest-carrying frames of the Falcon-512 cycle, bytes [NDSS 2024 Fig. 4].
///
/// A whole frame, not a credential size; see the module documentation. Used as the default
/// for [`CertCycleParams::base_frame_bytes`] — the room a cycle frame costs before any
/// certificate fragment is added — which is how α comes out at the documented value.
pub const CYCLE_BASE_FRAME_BYTES: u32 = 204;

/// The first frame of the Falcon-512 cycle, bytes [NDSS 2024 Fig. 4].
///
/// Carried for cross-checking only; this model does not return frame totals.
pub const CYCLE_FIRST_FRAME_BYTES: u32 = 1_026;

/// The P2PCD response backoff's upper bound, milliseconds: uniform 0-250 ms, mean 125 ms
/// [IEEE 1609.2a-2017 §8.1-8.2.4 restated by NDSS 2024 §II-A].
pub const P2PCD_MAX_BACKOFF_MS: u64 = 250;

/// Whole-frame sizes for a signed BSM plus overhead, public key and signature, bytes
/// [NDSS 2024 Fig. 4, VERIFIED secondary].
///
/// Cross-check anchors for `v2xw-sec` and `v2xw-msg`; this model returns none of them. The
/// first exceeds nothing, the rest exceed the DSRC cap of 2,304 B, which is why the fully
/// hybrid design is only feasible with Falcon and the pure-PQ design not at all.
pub const FRAME_SIZE_ANCHORS: [(&str, u32); 5] = [
    ("ecdsa", 350),
    ("falcon-512", 2_435),
    ("xmss", 5_610),
    ("dilithium", 6_310),
    ("sphincs+", 15_844),
];

/// Learning-response completion times and fragment counts [NDSS 2024 §V-D].
///
/// `(scheme, milliseconds, fragments)`; the fragment count is stated only for SPHINCS+, for
/// which NDSS gives 8. Anchors for the calibration of
/// [`CertCycleParams::learning_response_fragments`], not values this model computes.
pub const LEARNING_RESPONSE_ANCHORS: [(&str, u64, Option<u16>); 4] = [
    ("falcon-512", 250, None),
    ("xmss", 375, None),
    ("dilithium", 500, None),
    ("sphincs+", 1_000, Some(8)),
];

/// The parameters of the Partially-Hybrid certificate cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertCycleParams {
    /// τ: how many SPDUs one certificate cycle has. Default [`CERT_CYCLE_SPDUS`].
    pub tau_spdus: u16,
    /// The cycle's duration, milliseconds. Default [`CERT_CYCLE_PERIOD_MS`].
    pub cycle_period_ms: u64,
    /// The hybrid certificate's size, bytes. Default
    /// [`FALCON512_HYBRID_CERTIFICATE_BYTES`].
    pub hybrid_cert_bytes: u32,
    /// What a cycle frame costs before a certificate fragment is added, bytes. Default
    /// [`CYCLE_BASE_FRAME_BYTES`].
    pub base_frame_bytes: u32,
    /// β: how many fragments a P2PCD learning response is split into.
    pub learning_response_fragments: u16,
    /// The P2PCD response backoff's upper bound, milliseconds. Default
    /// [`P2PCD_MAX_BACKOFF_MS`].
    pub p2pcd_max_backoff_ms: u64,
}

impl Default for CertCycleParams {
    fn default() -> Self {
        Self {
            tau_spdus: CERT_CYCLE_SPDUS,
            cycle_period_ms: CERT_CYCLE_PERIOD_MS,
            hybrid_cert_bytes: FALCON512_HYBRID_CERTIFICATE_BYTES,
            base_frame_bytes: CYCLE_BASE_FRAME_BYTES,
            learning_response_fragments: 1,
            p2pcd_max_backoff_ms: P2PCD_MAX_BACKOFF_MS,
        }
    }
}

/// What one SPDU of a certificate cycle carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "carries")]
pub enum CarriedCredential {
    /// One of the α certificate fragments.
    CertFragment {
        /// Which fragment, `0..alpha`.
        index: u16,
        /// Its size, bytes.
        bytes: u32,
    },
    /// The certificate's digest, as the ordinary omission rule sends.
    CertDigest {
        /// Its size, bytes ([`CERT_DIGEST_BYTES`]).
        bytes: u32,
    },
}

impl CarriedCredential {
    /// The credential bytes this SPDU carries.
    pub const fn bytes(&self) -> u32 {
        match *self {
            CarriedCredential::CertFragment { bytes, .. }
            | CarriedCredential::CertDigest { bytes } => bytes,
        }
    }
}

/// One SPDU of a certificate cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleSlot {
    /// Which SPDU of the cycle, `0..tau`.
    pub spdu: u16,
    /// What it carries.
    pub carries: CarriedCredential,
}

/// `fragmenter/cert-cycle-partial-hybrid`: the hybrid certificate spread over the first α
/// SPDUs of each five-SPDU cycle.
///
/// ```
/// use v2xw_net::frag::cert_cycle::{CertCyclePartialHybrid, DSRC_PAYLOAD_CAP_BYTES};
///
/// let f = CertCyclePartialHybrid::default();
/// // The documented anchor: a Falcon-512 hybrid certificate of 858 B needs α = 1.
/// assert_eq!(f.alpha(DSRC_PAYLOAD_CAP_BYTES).unwrap(), 1);
/// // …so the certificate travels in one frame and loss does not amplify.
/// assert_eq!(f.cycle_plan(DSRC_PAYLOAD_CAP_BYTES).unwrap().len(), 5);
/// ```
#[derive(Debug, Clone)]
pub struct CertCyclePartialHybrid {
    params: CertCycleParams,
    buffer: ReassemblyBuffer,
    card: ModelCard,
}

impl Default for CertCyclePartialHybrid {
    fn default() -> Self {
        Self::new(CertCycleParams::default())
    }
}

impl CertCyclePartialHybrid {
    /// The number of partly received certificates one node keeps state for.
    ///
    /// One per peer whose certificate is arriving; a node in dense traffic hears a few
    /// hundred neighbours, and a set lives for one cycle. Not a tunable — the cycle is short
    /// enough that the buffer does not bind — but it is a number, so the card's assumptions
    /// state it (invariant I-C3 is about parameters a scenario can set; an internal bound
    /// still has to be visible to a reader).
    const MAX_OPEN_CERTIFICATES: usize = 512;

    /// The model with the given parameters.
    pub fn new(params: CertCycleParams) -> Self {
        Self {
            buffer: ReassemblyBuffer::new(
                Self::MAX_OPEN_CERTIFICATES,
                Duration::from_millis(params.cycle_period_ms),
            ),
            params,
            card: card(),
        }
    }

    /// The model configured from a resolved parameter set (invariant I-C3).
    pub fn from_params(params: &ParamSet) -> Self {
        let d = CertCycleParams::default();
        let read_u32 = |name: &str, fallback: u32| {
            params
                .get_u64(name)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback)
        };
        let read_u16 = |name: &str, fallback: u16| {
            params
                .get_u64(name)
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(fallback)
        };
        Self::new(CertCycleParams {
            tau_spdus: read_u16("tau_spdus", d.tau_spdus),
            cycle_period_ms: params
                .get_u64("cycle_period_ms")
                .unwrap_or(d.cycle_period_ms),
            hybrid_cert_bytes: read_u32("hybrid_cert_bytes", d.hybrid_cert_bytes),
            base_frame_bytes: read_u32("base_frame_bytes", d.base_frame_bytes),
            learning_response_fragments: read_u16(
                "learning_response_fragments",
                d.learning_response_fragments,
            ),
            p2pcd_max_backoff_ms: params
                .get_u64("p2pcd_max_backoff_ms")
                .unwrap_or(d.p2pcd_max_backoff_ms),
        })
    }

    /// The parameters in force.
    pub const fn params(&self) -> &CertCycleParams {
        &self.params
    }

    /// The certificate bytes one cycle frame has room for, at a payload cap of `cap`.
    pub const fn fragment_budget(&self, cap: u32) -> u32 {
        cap.saturating_sub(self.params.base_frame_bytes)
    }

    /// α: how many fragments the hybrid certificate is split into at a payload cap of
    /// `cap`.
    ///
    /// `alpha = ceil(hybrid_cert_bytes / (cap - base_frame_bytes))`, at least 1.
    ///
    /// # Errors
    /// [`DropCause::Mtu`] when a cycle frame leaves no room for a fragment, and
    /// [`DropCause::CycleTooShort`] when α would exceed τ — the next cycle would start
    /// before the certificate finished, so a receiver could never assemble one.
    pub fn alpha(&self, cap: u32) -> core::result::Result<u16, DropCause> {
        self.alpha_for(self.params.hybrid_cert_bytes, cap)
    }

    /// α for a credential of `bytes` bytes; see [`CertCyclePartialHybrid::alpha`].
    ///
    /// # Errors
    /// As [`CertCyclePartialHybrid::alpha`].
    pub fn alpha_for(&self, bytes: u32, cap: u32) -> core::result::Result<u16, DropCause> {
        let budget = self.fragment_budget(cap);
        if budget == 0 {
            return Err(DropCause::Mtu {
                sdu_bytes: bytes,
                mtu: cap,
            });
        }
        let needed = bytes.div_ceil(budget).max(1);
        let alpha = u16::try_from(needed).unwrap_or(u16::MAX);
        if alpha > self.params.tau_spdus {
            return Err(DropCause::CycleTooShort {
                needed: alpha,
                tau: self.params.tau_spdus,
            });
        }
        Ok(alpha)
    }

    /// What each of the cycle's τ SPDUs carries at a payload cap of `cap`: a certificate
    /// fragment in the first α, the certificate's digest in the rest.
    ///
    /// # Errors
    /// As [`CertCyclePartialHybrid::alpha`].
    pub fn cycle_plan(&self, cap: u32) -> core::result::Result<Vec<CycleSlot>, DropCause> {
        let alpha = self.alpha(cap)?;
        let parts = split_equal(self.params.hybrid_cert_bytes, alpha);
        Ok((0..self.params.tau_spdus)
            .map(|spdu| CycleSlot {
                spdu,
                carries: if spdu < alpha {
                    CarriedCredential::CertFragment {
                        index: spdu,
                        bytes: parts[usize::from(spdu)],
                    }
                } else {
                    CarriedCredential::CertDigest {
                        bytes: CERT_DIGEST_BYTES,
                    }
                },
            })
            .collect())
    }

    /// Splits a credential — the body of [`Fragmenter::fragment`], callable without naming
    /// a context type.
    ///
    /// `sdu_bytes` is the **credential** being distributed (the hybrid certificate, or a
    /// P2PCD learning response), not a message: in this scheme the messages themselves are
    /// never fragmented. `mtu` is the payload cap of the radio in use —
    /// [`DSRC_PAYLOAD_CAP_BYTES`] or [`CV2X_PAYLOAD_CAP_BYTES`].
    ///
    /// # Errors
    /// As [`CertCyclePartialHybrid::alpha`].
    pub fn split(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        let alpha = self.alpha_for(sdu_bytes, mtu)?;
        Ok(split_equal(sdu_bytes, alpha)
            .into_iter()
            .enumerate()
            .map(|(i, payload_bytes)| FragmentDesc {
                sdu,
                index: i as u16,
                count: alpha,
                // No fragmentation header: the fragments ride in the SPDU's own certificate
                // field, whose 30 B of structure are part of the certificate size NDSS
                // accounts for (30 + pk + sig).
                header_bytes: 0,
                payload_bytes,
                kind: FragmentKind::CertFragment,
            })
            .collect())
    }

    /// β fragments of a P2PCD learning response of `bytes` bytes.
    ///
    /// The same split as a certificate cycle's, but the count is the configured β rather
    /// than one derived from the cap, because NDSS reports learning-response fragmentation
    /// by *completion time* (250 ms for Falcon, about 1,000 ms and 8 fragments for
    /// SPHINCS+) rather than by a size rule.
    pub fn learning_response_fragments(&self, sdu: SduId, bytes: u32) -> Vec<FragmentDesc> {
        let beta = self.params.learning_response_fragments.max(1);
        split_equal(bytes, beta)
            .into_iter()
            .enumerate()
            .map(|(i, payload_bytes)| FragmentDesc {
                sdu,
                index: i as u16,
                count: beta,
                header_bytes: 0,
                payload_bytes,
                kind: FragmentKind::CertFragment,
            })
            .collect()
    }

    /// How long a learning response takes to finish, given one uniform draw in `[0, 1)` per
    /// fragment.
    ///
    /// Each fragment waits `u_i * p2pcd_max_backoff_ms` before it is sent
    /// [IEEE 1609.2 §8; NDSS 2024 §IV]. **The draws are arguments**, not taken here: a model
    /// that reached for an RNG of its own would break the per-entity stream keying of
    /// ADR 0004 §3, and this way the card can declare `uses_rng: false` honestly. The node
    /// runtime draws them from [`v2xw_core::rng::RngDomain::Crypto`] keyed by
    /// [`v2xw_core::rng::EntityRef::Node`], which is the scope credential behaviour belongs
    /// to.
    ///
    /// The mean of one wait is half the bound, 125 ms, which is the figure NDSS quotes.
    pub fn learning_response_delay(&self, draws: &[f64]) -> Duration {
        let bound_ns = self.params.p2pcd_max_backoff_ms.saturating_mul(NS_PER_MS) as f64;
        let mut total = 0u64;
        for u in draws {
            let clamped = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
            // `round` is an IEEE-754 exact operation, so the nanoseconds are a function of
            // the draw and not of the platform's libm (ADR 0003).
            total = total.saturating_add((bound_ns * clamped).round() as u64);
        }
        Duration::from_nanos(total)
    }

    /// The mean P2PCD response backoff: half the configured bound, 125 ms by default.
    pub const fn mean_backoff(&self) -> Duration {
        Duration::from_millis(self.params.p2pcd_max_backoff_ms / 2)
    }

    /// How many partly received certificates this node is holding.
    pub fn open_reassemblies(&self) -> usize {
        self.buffer.open()
    }
}

impl Model for CertCyclePartialHybrid {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C> Fragmenter<C> for CertCyclePartialHybrid
where
    C: Ctx + ?Sized,
{
    fn fragment(
        &self,
        sdu: SduId,
        sdu_bytes: u32,
        mtu: u32,
    ) -> core::result::Result<Vec<FragmentDesc>, DropCause> {
        self.split(sdu, sdu_bytes, mtu)
    }

    fn reassemble(
        &mut self,
        ctx: &mut C,
        rx: NodeId,
        frag: &FragmentDesc,
        from: NodeId,
    ) -> ReassemblyOutcome {
        let now = ctx.now();
        let outcome = self.buffer.accept(now, from, frag);
        for retired in self.buffer.take_expired() {
            ctx.emit(FragRecord::new(now, rx, &retired));
        }
        if crate::frag::should_record(frag, &outcome) {
            ctx.emit(FragRecord::new(now, rx, &outcome));
        }
        outcome
    }

    fn overhead_bytes(&self) -> u32 {
        0
    }

    fn reassembly_timeout(&self) -> Option<Duration> {
        Some(Duration::from_millis(self.params.cycle_period_ms))
    }

    fn expire(&mut self, ctx: &mut C, rx: NodeId, now: SimTime) -> Vec<ReassemblyOutcome> {
        let retired = self.buffer.expire(now);
        for outcome in &retired {
            ctx.emit(FragRecord::new(now, rx, outcome));
        }
        retired
    }
}

/// The model card for `fragmenter/cert-cycle-partial-hybrid`.
fn card() -> ModelCard {
    let ndss = |what: &str| Source {
        kind: SourceKind::Paper,
        reference: format!(
            "Twardokus, Bindel, Rahbari, McCarthy, \"When Cryptography Needs a Hand: \
             Practical Post-Quantum Authentication for V2V Communications\", NDSS 2024 — {what}"
        ),
        accessed: Some("2026-09-17".to_string()),
        note: Some("VERIFIED (secondary); see R4 §E and R5 §B.6.".to_string()),
    };

    let mut card = ModelCard::new(
        FRAGMENTER_CERT_CYCLE_ID,
        Family::Fragmenter,
        "1.0.0",
        "The NDSS 2024 Partially-Hybrid scheme: BSMs keep ECDSA signatures and the hybrid \
         certificate is split into alpha fragments carried in the first alpha SPDUs of each \
         five-SPDU (500 ms) certificate cycle, the rest carrying the certificate digest. \
         P2PCD learning responses are split into beta fragments, each after a uniform \
         0-250 ms wait.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "fragment count".to_string(),
            latex_or_text: "alpha = ceil(cert_bytes / (cap - base_frame_bytes)), 1 <= alpha <= tau"
                .to_string(),
            notes: Some(
                "At the DSRC cap of 2,304 B with 204 B cycle frames, a Falcon-512 hybrid \
                 certificate of 858 B gives alpha = 1, which is the documented anchor. \
                 alpha > tau is refused: the next cycle would start before the certificate \
                 finished."
                    .to_string(),
            ),
        },
        Equation {
            name: "learning-response delay".to_string(),
            latex_or_text: "T = sum_i U_i(0, p2pcd_max_backoff_ms), mean beta * 125 ms".to_string(),
            notes: Some(
                "The uniform draws are supplied by the caller from its own RNG stream, so \
                 this model draws nothing (ADR 0004 §3)."
                    .to_string(),
            ),
        },
        amplification::equation(),
    ];
    card.parameters = vec![
        Parameter {
            name: "tau_spdus".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(CERT_CYCLE_SPDUS),
            range: Some(vec![serde_json::json!(1), serde_json::json!(50)]),
            source: ndss("§IV: the certificate cycle is tau = 5 SPDUs"),
            calibration: None,
        },
        Parameter {
            name: "cycle_period_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(CERT_CYCLE_PERIOD_MS),
            range: Some(vec![serde_json::json!(100), serde_json::json!(5_000)]),
            source: ndss(
                "§IV: tau = 5 SPDUs = 500 ms at the 10 Hz BSM rate; the industry certificate \
                 cadence is every fifth SPDU (R4 §E)",
            ),
            calibration: None,
        },
        Parameter {
            name: "hybrid_cert_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(FALCON512_HYBRID_CERTIFICATE_BYTES),
            range: Some(vec![serde_json::json!(1), serde_json::json!(65_535)]),
            source: ndss(
                "Fig. 4: the Falcon-512 hybrid certificate is 858 B; the general form is \
                 30 + pk + sig",
            ),
            calibration: None,
        },
        Parameter {
            name: "base_frame_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(CYCLE_BASE_FRAME_BYTES),
            range: Some(vec![serde_json::json!(0), serde_json::json!(2_304)]),
            source: ndss(
                "Fig. 4: the four digest-carrying frames of the Falcon-512 cycle are 204 B \
                 each, which is what a cycle frame costs before a certificate fragment",
            ),
            calibration: None,
        },
        Parameter {
            name: "learning_response_fragments".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(1),
            range: Some(vec![serde_json::json!(1), serde_json::json!(16)]),
            source: Source::todo_calibrate(
                "beta is reported by NDSS only through completion times (Falcon 250 ms, \
                 XMSS 375 ms, Dilithium about 500 ms, SPHINCS+ about 1,000 ms with 8 \
                 fragments); no size rule for beta was published, so the Falcon default of 1 \
                 is an inference from its 250 ms figure rather than a cited value.",
            ),
            calibration: Some(
                "Sweep beta for each PQ scheme and keep the value whose modelled completion \
                 time (beta waits of mean 125 ms plus the transmissions) matches the \
                 LEARNING_RESPONSE_ANCHORS times to within their stated precision; \
                 SPHINCS+'s beta = 8 is the one anchor that fixes the relationship."
                    .to_string(),
            ),
        },
        Parameter {
            name: "p2pcd_max_backoff_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(P2PCD_MAX_BACKOFF_MS),
            range: Some(vec![serde_json::json!(0), serde_json::json!(5_000)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "IEEE 1609.2a-2017 §8.1-8.2.4 — P2PCD response backoff, restated \
                            by NDSS 2024 §II-A as uniform 0-250 ms with a response threshold \
                            of 3"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The messages are never fragmented; only the credential is. A BSM keeps its ECDSA \
         signature and its ordinary size, which is what makes the scheme deployable."
            .to_string(),
        "The alpha fragments are equal in size, as NDSS describes, and are carried in the \
         first alpha SPDUs of the cycle. This model splits them to within one byte so that \
         they sum to exactly the certificate size."
            .to_string(),
        "A receiver needs every fragment of one cycle: the reassembly timeout is the cycle \
         period itself (500 ms), after which the next cycle's fragments begin and a partial \
         set is stale (invariant I-N2). That timeout is DERIVED from tau and the 10 Hz rate, \
         not quoted."
            .to_string(),
        "This model draws no random numbers. The P2PCD backoff's uniform draws are \
         arguments to learning_response_delay, supplied by the caller from its own \
         RngDomain::Crypto stream."
            .to_string(),
        "The reassembly buffer holds at most 512 partly received certificates per node, \
         which is not a scenario parameter: a set lives for one 500 ms cycle, so the bound \
         does not bind at any density this simulator runs. A peer whose certificate arrives \
         while 512 others are in flight is refused with DropCause::ReassemblyBufferFull \
         rather than evicting one of them."
            .to_string(),
    ];
    card.limitations = {
        let mut l = vec![
            "The NDSS frame totals (first frame 1,026 B, the other four 204 B) are whole \
             frames — MAC, envelope, BSM and credential together — and this model returns \
             credential sizes only. Reproducing them here would double-count bytes owned by \
             v2xw-msg, v2xw-sec and v2xw-radio, which invariant I-N1 forbids; they are kept \
             as constants for cross-checking."
                .to_string(),
            "beta, the learning-response fragment count, is not derived from a size rule \
             (see its calibration plan)."
                .to_string(),
            "The hybrid-certificate size formula '30 + pk + sig' and the 858 B Falcon-512 \
             anchor disagree: with Falcon-512's pk of 897 B the formula gives 1 593 B, while \
             858 B is 30 + 162 (the ECDSA certificate) + 666 (the Falcon signature). The \
             default is the 858 B Fig. 4 anchor and the other reading is a parameter \
             override; alpha is 1 at the DSRC cap under both."
                .to_string(),
            "P2PCD's own protocol — learning requests, the response-count threshold of 3, \
             the inline versus out-of-band flavours — is v2xw-proto's (03-interfaces.md §7, \
             04-models.md §9.5). This model supplies the fragmentation and the delay only."
                .to_string(),
            "At the C-V2X payload cap of 437 B the same certificate needs four fragments, \
             which fits a five-SPDU cycle arithmetically; NDSS's conclusion that C-V2X \
             cannot practically support PQC rests on the whole-frame sizes, which this model \
             does not compute, so it is not reproduced as a refusal here."
                .to_string(),
        ];
        l.extend(amplification::ASSUMPTIONS.iter().map(|s| (*s).to_string()));
        l
    };
    card.ignores = vec![
        "The cryptographic work itself: key sizes, signing and verification cost are \
         v2xw-sec's primitive descriptors (04-models.md §9.4)."
            .to_string(),
        "The fully hybrid (dual-signature) and pure-PQ designs, which NDSS finds infeasible \
         at the DSRC payload cap."
            .to_string(),
        "The 0.25-0.39 ms added per-BSM delay NDSS measures, which belongs to the node's \
         compute model (03-interfaces.md §8)."
            .to_string(),
    ];
    card.sources = vec![
        ndss("§II-B, §III, §IV, §V-D — the Partially-Hybrid design and its measurements"),
        Source::new(
            SourceKind::Standard,
            "IEEE 1609.2a-2017 §6.3.9, §8.1-8.2.4 — P2PCD, its backoff and its thresholds",
        ),
        Source::new(
            SourceKind::Paper,
            "docs/design/research/R4-messages-envelopes.md §E and R5 §B.6 — the fact sheets \
             these anchors were extracted from",
        ),
        Source::new(SourceKind::Paper, "04-models.md §7.3, §7.4, §9.5"),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![ndss(
            "Fig. 4 — alpha = 1 for a Falcon-512 hybrid certificate of 858 B at the DSRC cap",
        )],
        tests: vec![
            "frag::cert_cycle::tests::falcon_512_needs_one_fragment_as_documented".to_string(),
            "frag::cert_cycle::tests::the_cycle_carries_fragments_then_digests".to_string(),
            "frag::cert_cycle::tests::alpha_above_tau_is_refused".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amplification::{FragmentLoss, equal_p_loss};
    use crate::testctx::TestCtx;

    /// The documented anchor: 858 B of Falcon-512 hybrid certificate, α = 1.
    #[test]
    fn falcon_512_needs_one_fragment_as_documented() {
        let f = CertCyclePartialHybrid::default();
        assert_eq!(f.params().hybrid_cert_bytes, 858);
        assert_eq!(FALCON512_HYBRID_CERTIFICATE_BYTES, 858);
        assert_eq!(f.alpha(DSRC_PAYLOAD_CAP_BYTES), Ok(1));

        // …and with α = 1 the certificate is one fragment of the whole certificate.
        let parts = f.split(SduId::new(1), 858, DSRC_PAYLOAD_CAP_BYTES).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].payload_bytes, 858);
        assert_eq!(parts[0].kind, FragmentKind::CertFragment);
        assert_eq!(parts[0].header_bytes, 0);

        // The documented frame anchors are carried, unmodified, for cross-checking.
        assert_eq!(CYCLE_FIRST_FRAME_BYTES, 1_026);
        assert_eq!(CYCLE_BASE_FRAME_BYTES, 204);
        assert_eq!(ECDSA_CERTIFICATE_BYTES, 162);
        assert_eq!(FRAME_SIZE_ANCHORS[1], ("falcon-512", 2_435));
        assert!(
            FRAME_SIZE_ANCHORS[1].1 > DSRC_PAYLOAD_CAP_BYTES,
            "which is why the fully hybrid design does not fit"
        );
    }

    /// §7.4's point about this design: α = 1 keeps n = 1, so the certificate suffers no
    /// amplification at all, while a five-fragment cycle would lose it 41 % of the time at
    /// p = 0.1.
    #[test]
    fn alpha_one_means_no_amplification() {
        let f = CertCyclePartialHybrid::default();
        let one = <CertCyclePartialHybrid as Fragmenter<TestCtx>>::loss_amplification(
            &f,
            &[FragmentLoss::new(0, 0.1, 858)],
        );
        assert!((one.p_sdu_lost() - 0.1).abs() < 1e-15);

        let five: Vec<FragmentLoss> = (0..5).map(|i| FragmentLoss::new(i, 0.1, 172)).collect();
        let m = <CertCyclePartialHybrid as Fragmenter<TestCtx>>::loss_amplification(&f, &five);
        assert!((m.p_sdu_lost() - equal_p_loss(5, 0.1)).abs() < 1e-15);
        assert!((m.p_sdu_lost() - 0.40951).abs() < 1e-12);
        assert!(m.amplifies, "every fragment of a certificate is needed");
    }

    #[test]
    fn the_cycle_carries_fragments_then_digests() {
        let f = CertCyclePartialHybrid::default();
        let plan = f.cycle_plan(DSRC_PAYLOAD_CAP_BYTES).unwrap();
        assert_eq!(plan.len(), 5, "tau = 5 SPDUs");
        assert_eq!(
            plan[0].carries,
            CarriedCredential::CertFragment {
                index: 0,
                bytes: 858
            }
        );
        for slot in &plan[1..] {
            assert_eq!(
                slot.carries,
                CarriedCredential::CertDigest { bytes: 8 },
                "the remaining SPDUs carry the certificate hash"
            );
        }
        // The fragments in a cycle sum to exactly the certificate.
        let carried: u32 = plan
            .iter()
            .filter_map(|s| match s.carries {
                CarriedCredential::CertFragment { bytes, .. } => Some(bytes),
                CarriedCredential::CertDigest { .. } => None,
            })
            .sum();
        assert_eq!(carried, 858);

        // A tighter cap spreads the certificate over more SPDUs.
        let plan = f.cycle_plan(CV2X_PAYLOAD_CAP_BYTES).unwrap();
        let fragments: Vec<u32> = plan
            .iter()
            .filter_map(|s| match s.carries {
                CarriedCredential::CertFragment { bytes, .. } => Some(bytes),
                CarriedCredential::CertDigest { .. } => None,
            })
            .collect();
        // 437 - 204 = 233 B per fragment, so ceil(858 / 233) = 4.
        assert_eq!(fragments.len(), 4);
        assert_eq!(fragments.iter().sum::<u32>(), 858);
        assert_eq!(plan.len(), 5);
    }

    #[test]
    fn alpha_above_tau_is_refused() {
        let f = CertCyclePartialHybrid::default();
        // A cap that leaves 96 B per frame needs nine fragments for 858 B.
        assert_eq!(
            f.alpha(300),
            Err(DropCause::CycleTooShort { needed: 9, tau: 5 })
        );
        // A cap at or below the frame's own cost leaves no room at all.
        assert_eq!(
            f.alpha(204),
            Err(DropCause::Mtu {
                sdu_bytes: 858,
                mtu: 204
            })
        );
    }

    #[test]
    fn the_fragments_of_a_cycle_reassemble_at_the_receiver() {
        let mut f = CertCyclePartialHybrid::new(CertCycleParams {
            hybrid_cert_bytes: 858,
            ..CertCycleParams::default()
        });
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(4);
        let rx = NodeId::new(1);
        // At the C-V2X cap the certificate takes four fragments, one per SPDU.
        let parts = f.split(SduId::new(2), 858, CV2X_PAYLOAD_CAP_BYTES).unwrap();
        assert_eq!(parts.len(), 4);
        for (i, part) in parts.iter().enumerate() {
            ctx.set_now(i as u64 * 100 * NS_PER_MS);
            let out = f.reassemble(&mut ctx, rx, part, peer);
            if i < 3 {
                assert!(matches!(out, ReassemblyOutcome::Pending { .. }), "{i}");
            } else {
                assert_eq!(
                    out,
                    ReassemblyOutcome::Complete {
                        sdu: SduId::new(2),
                        bytes: 858,
                        segments: 4
                    }
                );
            }
        }
        assert_eq!(f.open_reassemblies(), 0);
    }

    /// The reassembly timeout is the cycle itself: a set that is still incomplete when the
    /// next cycle begins is stale.
    #[test]
    fn a_partial_cycle_expires_after_the_cycle_period() {
        let mut f = CertCyclePartialHybrid::default();
        assert_eq!(
            <CertCyclePartialHybrid as Fragmenter<TestCtx>>::reassembly_timeout(&f),
            Some(Duration::from_millis(500))
        );
        let mut ctx = TestCtx::new();
        let peer = NodeId::new(4);
        let rx = NodeId::new(1);
        let parts = f.split(SduId::new(3), 858, CV2X_PAYLOAD_CAP_BYTES).unwrap();
        f.reassemble(&mut ctx, rx, &parts[0], peer);
        f.reassemble(&mut ctx, rx, &parts[1], peer);

        ctx.set_now(500 * NS_PER_MS);
        let retired = f.expire(&mut ctx, rx, 500 * NS_PER_MS);
        assert_eq!(retired.len(), 1);
        assert!(matches!(
            retired[0],
            ReassemblyOutcome::Expired {
                have: 2,
                want: 4,
                ..
            }
        ));
        assert!(
            ctx.records_on("net.frag")
                .iter()
                .any(|j| j.contains("\"outcome\":\"expired\""))
        );
    }

    #[test]
    fn the_learning_response_delay_is_the_sum_of_its_draws() {
        let f = CertCyclePartialHybrid::default();
        assert_eq!(f.mean_backoff(), Duration::from_millis(125));
        // One fragment, a draw at the mean.
        assert_eq!(
            f.learning_response_delay(&[0.5]),
            Duration::from_millis(125)
        );
        // Eight fragments, the SPHINCS+ anchor's count, each at the mean: 1,000 ms — which
        // is the completion time NDSS reports for it.
        let draws = [0.5f64; 8];
        assert_eq!(
            f.learning_response_delay(&draws),
            Duration::from_millis(1_000)
        );
        assert_eq!(LEARNING_RESPONSE_ANCHORS[3], ("sphincs+", 1_000, Some(8)));
        // Bounds: every draw at 0 or 1.
        assert_eq!(f.learning_response_delay(&[0.0; 4]), Duration::ZERO);
        assert_eq!(
            f.learning_response_delay(&[1.0; 4]),
            Duration::from_millis(1_000)
        );
        assert_eq!(f.learning_response_delay(&[]), Duration::ZERO);

        // The response itself splits into beta fragments summing to its size.
        let f = CertCyclePartialHybrid::new(CertCycleParams {
            learning_response_fragments: 8,
            ..CertCycleParams::default()
        });
        let parts = f.learning_response_fragments(SduId::new(9), 1_030);
        assert_eq!(parts.len(), 8);
        assert_eq!(parts.iter().map(|p| p.payload_bytes).sum::<u32>(), 1_030);
    }

    #[test]
    fn the_card_validates_and_its_uncited_default_has_a_plan() {
        let f = CertCyclePartialHybrid::default();
        let card = f.card();
        card.validate().expect("card validates (registry rule R1)");
        card.check_api_version().expect("api version matches");
        assert_eq!(card.family, Family::Fragmenter);
        assert!(
            !card.determinism.uses_rng,
            "the P2PCD draws are the caller's"
        );
        let todo: Vec<_> = card.todo_calibrate().collect();
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].name, "learning_response_fragments");
        assert!(todo[0].calibration.is_some());
        for a in amplification::ASSUMPTIONS {
            assert!(card.limitations.iter().any(|l| l == a));
        }
    }

    #[test]
    fn parameters_come_from_the_resolved_set() {
        let card = card();
        // 1,593 B is the other reading of NDSS's '30 + pk + sig' for Falcon-512
        // (30 + pk 897 + sig 666); see the module documentation.
        let params = ParamSet::resolve(&card, &serde_json::json!({"hybrid_cert_bytes": 1_593}))
            .expect("the override is declared and in range");
        let f = CertCyclePartialHybrid::from_params(&params);
        assert_eq!(f.params().hybrid_cert_bytes, 1_593);
        assert_eq!(
            f.alpha(DSRC_PAYLOAD_CAP_BYTES),
            Ok(1),
            "alpha is 1 under either reading, so the design's conclusion is unaffected"
        );
        assert_eq!(
            30 + ECDSA_CERTIFICATE_BYTES + 666,
            858,
            "and 858 is the other one"
        );
    }
}
