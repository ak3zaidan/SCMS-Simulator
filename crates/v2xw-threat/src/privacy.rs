//! The privacy observer: an adversary that never transmits, only listens, and measures how
//! long it can follow a vehicle across pseudonym changes (07-threats-and-detection.md §6).
//!
//! # Why this exists
//!
//! The whole pseudonym scheme — twenty concurrent certificates a week, the change
//! strategies of 05-protocols.md §2.4, the silent periods, the mix zones, the butterfly key
//! expansion that makes the certificates unlinkable at issuance — is there to defeat
//! exactly one adversary: someone with receivers by the road who writes down what it hears.
//! Until something in the simulator *is* that adversary, every pseudonym parameter is
//! unfalsifiable: a run can change the change interval from 5 minutes to 5 seconds and no
//! number anywhere moves.
//!
//! This model is that adversary, and it emits the four quantities
//! 08-measurement-and-data.md §2.6 names: `linkability_rate`, `anonymity_set_size`,
//! `degree_of_anonymity` and `tracking_duration`.
//!
//! # What it can see
//!
//! Receptions. Nothing else. It has no credentials, transmits nothing, and every field it
//! reads is on an [`ObservedMessage`] — which is the same firewall every detector in this
//! crate sits behind, and it matters more here than anywhere: an observer that could ask
//! which vehicle a digest belonged to would report perfect tracking and prove nothing.
//!
//! # What it cannot know, and who supplies it
//!
//! **Whether a link is correct.** That is the ground-truth join, and this model never
//! makes it: [`crate::records::PrivacyLinkClaim`] carries the two digests and the
//! observer's own posterior, and the run declares the digest-to-actor map on the
//! ground-truth side exactly as `v2xw_metrics::detection::DetectionProvider::declare_subject`
//! already does for reports. `linkability_rate` is then correct links over pseudonym
//! changes, with the denominator coming from the `sec.cert` `change` events — which is why
//! the observer also emits a record when it *declines* to link, so the denominator is not
//! silently the numerator.
//!
//! # The tracker
//!
//! A constant-velocity predictor with a covariance that grows over a silence, and a
//! best-hypothesis association — the method of Wiedersheim, Ma, Kargl and Papadimitratos,
//! *Privacy in inter-vehicular networks: why simple pseudonym change is not enough*
//! (WONS 2010), whose result 07-threats-and-detection.md §6 cites: at 1 Hz beacons a change
//! interval of 4 s and 20 % penetration already yields near-100 % tracking success. A
//! simulator that cannot reproduce that finding is not measuring privacy.
//!
//! Every constant in the association is cited: the gate is `z · σ` with the legacy
//! `detector_z_threshold` of 3.0 and the legacy `consistency_threshold_m` of 5.0 as σ's
//! floor, the covariance grows by the plausible-acceleration bound of 04-models.md §14
//! (3 m/s²) over the silence, and the silence itself is bounded by the longest cited
//! silent period, PRESERVE's 13 s (05-protocols.md §2.4).
//!
//! # Anonymity
//!
//! Ψ is the candidate set the observer could not tell apart: the predecessors whose
//! prediction reaches the new pseudonym's first claim. The observer's posterior over Ψ
//! gives the effective set size `S = −Σ p·log₂ p` and the degree of anonymity
//! `d = S / log₂|Ψ|` of ETSI TR 103 415 §5.1.2. `d = 0` with a single candidate is the
//! honest answer — there was nobody to hide among — and `d = 1` means the observer learned
//! nothing at all.

use std::collections::BTreeMap;

use crate::cards::{LEGACY_PY, design, legacy_param, paper, standard};
use crate::ctx::{ThreatCtx, ThreatCtxExt};
use crate::obs::{ObservedMessage, SelfBelief, VerificationState};
use crate::records::{PrivacyLinkClaim, PrivacyTrackSegment, q};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::time::{SimTime, ns_to_secs, secs_to_ns};

/// The model id this observer's card and every record it writes carry.
pub const MODEL_ID: &str = "threat/observer/passive-privacy";

/// The tracker's name, on every [`PrivacyLinkClaim::method`] it writes.
pub const METHOD: &str = "kinematic-best-hypothesis";

/// The name a declined link carries instead of a method.
pub const METHOD_UNLINKED: &str = "unlinked";

/// What the observer reads. Every default is cited.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserverParams {
    /// How many sigmas of predicted uncertainty the association gate spans
    /// (the legacy `detector_z_threshold`, 3.0).
    pub z_gate: f64,
    /// The floor on the position uncertainty, metres (the legacy
    /// `consistency_threshold_m`, 5.0).
    ///
    /// A message broadcasts its own confidence and the observer uses it; this is what it
    /// falls back to when a sender understates it, so a vehicle cannot evade tracking by
    /// claiming a confidence of zero.
    pub sigma_floor_m: f64,
    /// The plausible-acceleration bound the prediction covariance grows by, m/s²
    /// (F2MD `MIN_MAX_ACCEL`, 04-models.md §14).
    ///
    /// Over a silence of `Δt` the unmodelled displacement is at most `a·Δt²/2`, which is
    /// what makes a long silent period genuinely hard to track across instead of a
    /// constant-velocity guess that always lands.
    pub accel_bound_mps2: f64,
    /// How long a vanished pseudonym stays a linkage candidate, seconds.
    ///
    /// PRESERVE's silent period is 3–13 s (05-protocols.md §2.4, cited there), so 13 s is
    /// the longest silence a cited strategy produces and therefore the longest gap an
    /// observer has any reason to bridge.
    pub max_silence_s: f64,
    /// How many receptions a track needs before its velocity estimate is used.
    ///
    /// Two: one reception gives a position and no velocity. Structural, not tuned.
    pub min_fixes_for_velocity: u32,
    /// Whether the observer tracks frames whose signature it could not verify.
    ///
    /// `true` by default, because a passive observer need not hold a trust anchor at all
    /// and a tracker does not care whether a claim is honest. Setting it `false` models an
    /// observer that only follows verified traffic — and the difference between the two is
    /// how much a decoy transmitter buys a vehicle's privacy.
    pub track_unverifiable: bool,
}

impl Default for ObserverParams {
    fn default() -> Self {
        Self {
            z_gate: 3.0,
            sigma_floor_m: 5.0,
            accel_bound_mps2: 3.0,
            max_silence_s: 13.0,
            min_fixes_for_velocity: 2,
            track_unverifiable: true,
        }
    }
}

/// One pseudonym the observer is following, and the chain it believes it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Track {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    conf_m: f64,
    last_seen: SimTime,
    fixes: u32,
    /// The first pseudonym of the chain this track is the current end of.
    origin: [u8; 8],
    /// When the observer first heard the chain.
    chain_start: SimTime,
    /// How many pseudonym changes it has followed to get here.
    links: u32,
    /// How many receptions the whole chain is built from.
    chain_fixes: u32,
}

/// What the observer concluded about one new pseudonym.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkOutcome {
    /// The new pseudonym digest, hex.
    pub successor: String,
    /// The predecessor it linked to, hex, or `None` when it declined.
    pub predecessor: Option<String>,
    /// Its posterior for that predecessor, in `[0, 1]`.
    pub posterior: f64,
    /// |Ψ|: how many candidates it could not tell apart.
    pub anonymity_set_size: u32,
    /// S = −Σ p·log₂ p, in bits.
    pub effective_anonymity_set_bits: f64,
    /// d = S / log₂|Ψ|, or `0` when |Ψ| ≤ 1.
    pub degree_of_anonymity: f64,
}

/// A passive adversary that measures how long it can follow a vehicle.
#[derive(Debug, Clone)]
pub struct PrivacyObserver {
    card: ModelCard,
    params: ObserverParams,
    node: NodeId,
    tracks: BTreeMap<[u8; 8], Track>,
    links: u64,
    declined: u64,
    closed: u64,
    longest_chain_links: u32,
    longest_track_s: f64,
    skipped_unverifiable: u64,
    deferred: u64,
}

impl PrivacyObserver {
    /// An observer hosted at `node`.
    ///
    /// `node` is the receiver it listens from: an RSU-class receiver at a roadside site,
    /// as 07-threats-and-detection.md §6 places them, or a parked vehicle. Its coverage is
    /// the node's own receiver range, so the observer density a study varies is a scenario
    /// property and not a parameter here.
    #[must_use]
    pub fn new(node: NodeId, params: ObserverParams) -> Self {
        Self {
            card: card(&params),
            params,
            node,
            tracks: BTreeMap::new(),
            links: 0,
            declined: 0,
            closed: 0,
            longest_chain_links: 0,
            longest_track_s: 0.0,
            skipped_unverifiable: 0,
            deferred: 0,
        }
    }

    /// An observer with the cited defaults.
    #[must_use]
    pub fn cited_defaults(node: NodeId) -> Self {
        Self::new(node, ObserverParams::default())
    }

    /// The parameters it reads.
    #[must_use]
    pub fn params(&self) -> &ObserverParams {
        &self.params
    }

    /// How many pseudonyms it is currently following.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.tracks.len()
    }

    /// How many links it has claimed.
    #[must_use]
    pub fn links_claimed(&self) -> u64 {
        self.links
    }

    /// How many new pseudonyms it declined to link: the scheme working.
    #[must_use]
    pub fn links_declined(&self) -> u64 {
        self.declined
    }

    /// How many chains it has closed.
    #[must_use]
    pub fn chains_closed(&self) -> u64 {
        self.closed
    }

    /// The most pseudonym changes it has followed in one chain.
    #[must_use]
    pub fn longest_chain_links(&self) -> u32 {
        self.longest_chain_links
    }

    /// The longest chain duration it has closed, seconds.
    #[must_use]
    pub fn longest_track_s(&self) -> f64 {
        self.longest_track_s
    }

    /// How many frames it ignored because it does not track unverifiable traffic.
    #[must_use]
    pub fn skipped_unverifiable(&self) -> u64 {
        self.skipped_unverifiable
    }

    /// How many frames arrived with verification deferred.
    ///
    /// Counted separately from [`Self::skipped_unverifiable`] because the two are
    /// different facts: "this node's policy had not checked" is not "the check failed",
    /// and an observer that conflated them would report a tracking rate that depended on
    /// a verification policy it does not even run.
    #[must_use]
    pub fn deferred(&self) -> u64 {
        self.deferred
    }

    /// How many pseudonym changes the observer has followed for the chain `digest` is the
    /// current end of, and how long that chain has lasted.
    #[must_use]
    pub fn chain_of(&self, digest: &[u8; 8]) -> Option<(u32, f64)> {
        self.tracks.get(digest).map(|t| {
            (
                t.links,
                ns_to_secs(t.last_seen.saturating_sub(t.chain_start)),
            )
        })
    }

    /// Whether this frame is one the observer will track at all.
    ///
    /// All four verification states, explicitly. They are four different facts and an
    /// observer that treated them alike would measure something else:
    ///
    /// * `Valid` — verified: tracked.
    /// * `Unverified` — the *hosting node's* policy deferred the check. Nothing is known
    ///   about the signature, so nothing follows about trackability: tracked, and counted,
    ///   because a reader needs to know how much of a tracking result rests on unverified
    ///   frames.
    /// * `BadSignature` — tested and failed. A passive tracker does not care in principle,
    ///   but a forged frame may be a decoy planted to break the association, so whether it
    ///   is tracked is the declared policy [`ObserverParams::track_unverifiable`].
    /// * `UnknownCertificate` — not a failure at all: this receiver has not learned the
    ///   signer's authority. The same policy applies, and the count is separate from a
    ///   deferred check.
    fn will_track(&mut self, m: &ObservedMessage) -> bool {
        match m.verification {
            VerificationState::Valid => true,
            VerificationState::Unverified => {
                self.deferred += 1;
                true
            }
            VerificationState::BadSignature | VerificationState::UnknownCertificate => {
                if self.params.track_unverifiable {
                    true
                } else {
                    self.skipped_unverifiable += 1;
                    false
                }
            }
        }
    }

    /// The predicted position of a track at `t`, and the prediction's sigma.
    ///
    /// `σ(Δt) = √(σ₀² + (a·Δt²/2)²)`: the broadcast confidence, grown by the displacement
    /// a plausible acceleration could have produced over the silence. It is what makes a
    /// long silent period hard to bridge rather than a constant-velocity guess that always
    /// lands somewhere.
    fn predict(&self, tr: &Track, t: SimTime) -> (f64, f64, f64) {
        let dt = ns_to_secs(t.saturating_sub(tr.last_seen));
        let (vx, vy) = if tr.fixes >= self.params.min_fixes_for_velocity {
            (tr.vx, tr.vy)
        } else {
            (0.0, 0.0)
        };
        let sigma0 = tr.conf_m.max(self.params.sigma_floor_m);
        let slack = 0.5 * self.params.accel_bound_mps2 * dt * dt;
        let sigma = math::sqrt(sigma0 * sigma0 + slack * slack);
        (tr.x + vx * dt, tr.y + vy * dt, sigma)
    }

    /// Closes every chain that has been silent longer than the observer's gate.
    ///
    /// A chain the observer gave up on is a `privacy.track` segment: that is the
    /// `tracking_duration` sample of 08-measurement-and-data.md §2.6.
    pub fn sweep(&mut self, ctx: &mut dyn ThreatCtx, t: SimTime) {
        let window = secs_to_ns(self.params.max_silence_s);
        let dead: Vec<[u8; 8]> = self
            .tracks
            .iter()
            .filter(|(_, tr)| t.saturating_sub(tr.last_seen) > window)
            .map(|(d, _)| *d)
            .collect();
        for digest in dead {
            self.close(ctx, digest, "silence");
        }
    }

    /// Closes every chain the observer still holds, at the end of a run.
    pub fn finish(&mut self, ctx: &mut dyn ThreatCtx) {
        let all: Vec<[u8; 8]> = self.tracks.keys().copied().collect();
        for digest in all {
            self.close(ctx, digest, "run-end");
        }
    }

    fn close(&mut self, ctx: &mut dyn ThreatCtx, digest: [u8; 8], reason: &str) {
        let Some(tr) = self.tracks.remove(&digest) else {
            return;
        };
        let duration_s = ns_to_secs(tr.last_seen.saturating_sub(tr.chain_start));
        self.closed += 1;
        if duration_s > self.longest_track_s {
            self.longest_track_s = duration_s;
        }
        ctx.emit(PrivacyTrackSegment {
            t: tr.last_seen,
            observer: self.node,
            origin: v2xw_core::hash::hex_encode(&tr.origin),
            last: v2xw_core::hash::hex_encode(&digest),
            links: tr.links,
            duration_s: q(duration_s),
            fixes: tr.chain_fixes,
            closed_reason: reason.to_string(),
        });
    }

    /// Feeds one reception to the observer.
    ///
    /// Returns what it concluded when the reception was the **first** from a pseudonym —
    /// which is the only moment a linkage decision exists — and `None` for a reception
    /// that merely extends a track it already holds.
    ///
    /// `_me` is the hosting node's own belief. It is taken for symmetry with
    /// [`crate::detect::Detector::on_message`], and deliberately unused: the association
    /// reads the reception's own claim, confidence and arrival instant, so the observer's
    /// own position never enters it. That is not an oversight but the model — a listener
    /// at the roadside links two claims to each other, not to itself — and taking the
    /// argument keeps the observer hostable exactly where a detector is.
    pub fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        _me: &SelfBelief,
        m: &ObservedMessage,
    ) -> Option<LinkOutcome> {
        if !self.will_track(m) {
            return None;
        }
        // The observer's own clock, never the simulator's.
        let t = m.received_at;
        // A pseudonym it is already following: extend the track and decide nothing.
        if let Some(tr) = self.tracks.get(&m.signer).copied() {
            let dt = ns_to_secs(t.saturating_sub(tr.last_seen)).max(1e-3);
            let mut next = tr;
            next.vx = (m.claimed_x_m - tr.x) / dt;
            next.vy = (m.claimed_y_m - tr.y) / dt;
            next.x = m.claimed_x_m;
            next.y = m.claimed_y_m;
            next.conf_m = m.claimed_pos_confidence_m;
            next.last_seen = t;
            next.fixes = tr.fixes.saturating_add(1);
            next.chain_fixes = tr.chain_fixes.saturating_add(1);
            self.tracks.insert(m.signer, next);
            return None;
        }

        // A pseudonym it has never heard: the linkage decision.
        //
        // Candidates are the tracks whose last reception is strictly older than this one —
        // a pseudonym heard at this very instant cannot have become this one — and inside
        // the observer's silence gate. They are visited in digest order, so the posterior,
        // the tie-break and the record order are the same on every run and every thread
        // count, and the weight sum goes through `sum_ordered`.
        let window = secs_to_ns(self.params.max_silence_s);
        let mut candidates: Vec<([u8; 8], f64)> = Vec::new();
        for (digest, tr) in &self.tracks {
            if tr.last_seen >= t || t.saturating_sub(tr.last_seen) > window {
                continue;
            }
            let (px, py, sigma) = self.predict(tr, t);
            let r = math::hypot(m.claimed_x_m - px, m.claimed_y_m - py);
            if r > self.params.z_gate * sigma {
                continue;
            }
            let z = r / sigma.max(1e-9);
            candidates.push((*digest, math::exp(-0.5 * z * z)));
        }

        let total = math::sum_ordered(candidates.iter().map(|(_, w)| *w));
        let posteriors: Vec<([u8; 8], f64)> = if total > 0.0 {
            candidates.iter().map(|(d, w)| (*d, w / total)).collect()
        } else {
            Vec::new()
        };
        // S = −Σ p·log₂ p, summed in digest order.
        let entropy = -math::sum_ordered(
            posteriors
                .iter()
                .filter(|(_, p)| *p > 0.0)
                .map(|(_, p)| p * math::log2(*p)),
        );
        let set_size = u32::try_from(posteriors.len()).unwrap_or(u32::MAX);
        let degree = if set_size > 1 {
            entropy / math::log2(f64::from(set_size))
        } else {
            0.0
        };
        // The best hypothesis: the largest posterior, ties to the lower digest, which the
        // digest-ordered iteration gives for free.
        let best =
            posteriors
                .iter()
                .copied()
                .fold(None::<([u8; 8], f64)>, |acc, (d, p)| match acc {
                    Some((_, bp)) if bp >= p => acc,
                    _ => Some((d, p)),
                });

        let successor_hex = m.signer_hex();
        let outcome = match best {
            Some((predecessor, posterior)) => {
                // The chain continues: the successor inherits the predecessor's origin and
                // start, and the predecessor's own track is retired into it.
                let prev = self.tracks.remove(&predecessor);
                let (origin, chain_start, links, chain_fixes) =
                    prev.map_or((m.signer, t, 0, 0), |p| {
                        (
                            p.origin,
                            p.chain_start,
                            p.links.saturating_add(1),
                            p.chain_fixes,
                        )
                    });
                self.links += 1;
                if links > self.longest_chain_links {
                    self.longest_chain_links = links;
                }
                self.insert_track(m, t, origin, chain_start, links, chain_fixes);
                let predecessor_hex = v2xw_core::hash::hex_encode(&predecessor);
                ctx.emit(PrivacyLinkClaim {
                    t,
                    observer: self.node,
                    predecessor: predecessor_hex.clone(),
                    successor: successor_hex.clone(),
                    posterior: q(posterior),
                    candidates: set_size,
                    anonymity_set_size: set_size,
                    effective_anonymity_set_bits: q(entropy),
                    degree_of_anonymity: q(degree),
                    method: METHOD.to_string(),
                });
                LinkOutcome {
                    successor: successor_hex,
                    predecessor: Some(predecessor_hex),
                    posterior: q(posterior),
                    anonymity_set_size: set_size,
                    effective_anonymity_set_bits: q(entropy),
                    degree_of_anonymity: q(degree),
                }
            }
            None => {
                // Nothing reached it: a new chain starts here. The record is still written,
                // because a linkage rate needs its denominator.
                self.declined += 1;
                self.insert_track(m, t, m.signer, t, 0, 0);
                ctx.emit(PrivacyLinkClaim {
                    t,
                    observer: self.node,
                    predecessor: String::new(),
                    successor: successor_hex.clone(),
                    posterior: 0.0,
                    candidates: 0,
                    anonymity_set_size: 0,
                    effective_anonymity_set_bits: 0.0,
                    degree_of_anonymity: 0.0,
                    method: METHOD_UNLINKED.to_string(),
                });
                LinkOutcome {
                    successor: successor_hex,
                    predecessor: None,
                    posterior: 0.0,
                    anonymity_set_size: 0,
                    effective_anonymity_set_bits: 0.0,
                    degree_of_anonymity: 0.0,
                }
            }
        };
        Some(outcome)
    }

    fn insert_track(
        &mut self,
        m: &ObservedMessage,
        t: SimTime,
        origin: [u8; 8],
        chain_start: SimTime,
        links: u32,
        chain_fixes: u32,
    ) {
        self.tracks.insert(
            m.signer,
            Track {
                x: m.claimed_x_m,
                y: m.claimed_y_m,
                vx: 0.0,
                vy: 0.0,
                conf_m: m.claimed_pos_confidence_m,
                last_seen: t,
                fixes: 1,
                origin,
                chain_start,
                links,
                chain_fixes: chain_fixes.saturating_add(1),
            },
        );
    }
}

impl Model for PrivacyObserver {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// The model card for the observer.
#[must_use]
pub fn card(p: &ObserverParams) -> ModelCard {
    use serde_json::json;
    let wons = paper(
        "F. Wiedersheim, Z. Ma, F. Kargl and P. Papadimitratos, Privacy in \
         inter-vehicular networks: why simple pseudonym change is not enough, WONS 2010 \
         (cited in 07-threats-and-detection.md §6)",
    );
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Attacker,
        "1.0.0",
        "A passive adversary that transmits nothing and links pseudonyms across changes by \
         kinematic continuity, reporting linkability, anonymity-set size, degree of \
         anonymity and tracking duration.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "prediction",
            "p̂(t) = p + v·Δt; σ(Δt) = √(σ₀² + (a·Δt²/2)²) with σ₀ = max(broadcast \
             confidence, floor) and a the plausible-acceleration bound",
        ),
        Equation::new(
            "association gate",
            "a candidate is kept when ‖claim − p̂‖ ≤ z·σ(Δt)",
        ),
        Equation::new(
            "posterior",
            "w_u = exp(−½ (r_u/σ_u)²); p_u = w_u / Σ w (summed in digest order via \
             sum_ordered)",
        ),
        Equation::new(
            "anonymity",
            "S = −Σ p_u·log₂ p_u; d = S / log₂|Ψ| for |Ψ| > 1, else 0 \
             (ETSI TR 103 415 §5.1.2)",
        ),
        Equation::new(
            "tracking duration",
            "for each chain the observer gives up on: last reception − first reception of \
             the chain (the WONS 2010 method)",
        ),
    ];
    card.parameters = vec![
        legacy_param(
            "z_gate",
            "-",
            json!(p.z_gate),
            LEGACY_PY,
            "PipelineConfig.detector_z_threshold",
        ),
        legacy_param(
            "sigma_floor_m",
            "m",
            json!(p.sigma_floor_m),
            LEGACY_PY,
            "PipelineConfig.consistency_threshold_m",
        ),
        Parameter::new(
            "accel_bound_mps2",
            "m/s^2",
            json!(p.accel_bound_mps2),
            design("04-models.md §14 (F2MD MIN_MAX_ACCEL)"),
        ),
        Parameter::new(
            "max_silence_s",
            "s",
            json!(p.max_silence_s),
            design(
                "05-protocols.md §2.4 (PRESERVE's 3–13 s silent period, the longest cited \
                 silence a change strategy produces)",
            ),
        ),
        Parameter::new(
            "min_fixes_for_velocity",
            "-",
            json!(p.min_fixes_for_velocity),
            Source {
                kind: SourceKind::Code,
                reference: "structural: one reception gives a position and no velocity".to_string(),
                accessed: Some(crate::cards::LEGACY_ACCESSED.to_string()),
                note: None,
            },
        ),
        Parameter::new(
            "track_unverifiable",
            "-",
            json!(p.track_unverifiable),
            Source {
                kind: SourceKind::Code,
                reference: "07-threats-and-detection.md §6 (observer knowledge: none beyond \
                            its own receptions)"
                    .to_string(),
                accessed: Some(crate::cards::LEGACY_ACCESSED.to_string()),
                note: Some(
                    "a passive observer need hold no trust anchor, so by default it tracks \
                     what it hears"
                        .to_string(),
                ),
            },
        ),
    ];
    card.sources = vec![
        design("07-threats-and-detection.md §6 (observer model, the four privacy metrics)"),
        design(
            "08-measurement-and-data.md §2.6 (linkability_rate, anonymity_set_size, \
                degree_of_anonymity, tracking_duration)",
        ),
        design("05-protocols.md §2.4 (the change strategies this observer is run against)"),
        wons.clone(),
        standard("ETSI TR 103 415 §5.1.2 (effective anonymity-set size, degree of anonymity)"),
        standard(
            "ETSI TR 103 415 §8 (the Sybil surface grows with the concurrent-pseudonym \
                  count, reported alongside)",
        ),
    ];
    card.assumptions = vec![
        "It transmits nothing and reads only its own receptions, so a tracking result here \
         is one a real roadside listener could obtain (invariant I-T1)."
            .to_string(),
        "Whether a claimed link is correct is not computed here: the record carries the two \
         digests and the posterior, and the run declares the digest-to-actor map."
            .to_string(),
        "A candidate heard at the same instant as the new pseudonym is excluded, because \
         one radio cannot be two pseudonyms at one instant — which is exactly what a Sybil \
         attacker does, and why a Sybil run's linkability is not comparable with an honest \
         run's."
            .to_string(),
        "The observer's coverage is the hosting node's own receiver range; observer density \
         is a scenario property."
            .to_string(),
    ];
    card.limitations = vec![
        "Best-hypothesis association, not full multi-hypothesis tracking: the observer \
         commits to the largest posterior rather than carrying a hypothesis tree forward. \
         WONS 2010 uses MHT, so this port is a lower bound on tracking success, and the \
         posterior it records is what an MHT would have started from."
            .to_string(),
        "No map matching: an observer that knew the road network would rule out candidates \
         a constant-velocity prediction admits, so a mapped observer tracks better than \
         this one. Declared as a capability (Knowledge::map) and not implemented here."
            .to_string(),
        "Mix zones and silent periods are modelled only through the silence gate: a vehicle \
         that stops transmitting for longer than max_silence_s is unlinkable by \
         construction here, which makes the gate the most consequential parameter in the \
         model and the one most in need of the WONS 2010 replication."
            .to_string(),
        "The four privacy metrics are emitted as records; the aggregation \
         (08-measurement-and-data.md §2.6) belongs to a metric provider in v2xw-metrics, \
         which has no privacy provider yet — the channels privacy.link and privacy.track \
         are new with this model."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![wons],
        tests: vec![
            "privacy::a_pseudonym_change_with_no_silence_is_linked".to_string(),
            "privacy::a_long_enough_silent_period_defeats_the_observer".to_string(),
            "privacy::a_crowd_leaves_the_observer_uncertain".to_string(),
            "privacy::the_observer_emits_a_record_when_it_declines".to_string(),
        ],
    };
    card.cost = Some(v2xw_core::card::CostClass {
        per_call_us: None,
        notes: Some(
            "Not measured, and not charged to any node: the observer is an adversary's \
             equipment, not a simulated node's CPU."
                .to_string(),
        ),
    });
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cited_defaults_are_the_cited_numbers() {
        let p = ObserverParams::default();
        assert_eq!(p.z_gate, 3.0);
        assert_eq!(p.sigma_floor_m, 5.0);
        assert_eq!(p.accel_bound_mps2, 3.0);
        assert_eq!(p.max_silence_s, 13.0);
        assert_eq!(p.min_fixes_for_velocity, 2);
        assert!(p.track_unverifiable);
    }

    #[test]
    fn the_card_validates_and_every_default_is_cited_or_planned() {
        let c = card(&ObserverParams::default());
        c.validate().unwrap();
        c.check_api_version().unwrap();
        assert!(!c.determinism.uses_rng, "the observer draws nothing");
        for p in &c.parameters {
            let cited = p.source.kind != SourceKind::TodoCalibrate;
            let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
            assert!(cited || planned, "{}", p.name);
        }
    }

    #[test]
    fn the_degree_of_anonymity_is_zero_with_nobody_to_hide_among() {
        // The honest answer, not a missing value: |Ψ| = 1 means there was no crowd.
        let o = PrivacyObserver::cited_defaults(NodeId::new(1));
        assert_eq!(o.tracked(), 0);
        assert_eq!(o.links_claimed(), 0);
        assert_eq!(o.chain_of(&[0; 8]), None);
    }
}
