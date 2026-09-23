//! **Worked example — the `Detector` seam.** A heading-rate plausibility check.
//!
//! This is the example the tutorial (`docs/site/content/tutorial.md`) walks end to end,
//! because a detector is what the roadmap's Phase 4 acceptance criterion is about: *a
//! researcher outside the team adds a detector plug-in from the tutorial in under a day.*
//!
//! # What it checks
//!
//! A vehicle cannot turn arbitrarily fast. So: keep the last heading each signer claimed,
//! and when the next beacon arrives, score the heading change against what is plausible
//! over the interval the two beacons claim to span.
//!
//! ```text
//! score = |angle(heading_now − heading_before)| / max_heading_change
//! ```
//!
//! normalised so that **score ≈ 1 is the firing threshold**, which is the contract every
//! detector in this project keeps (`detnorm ≈ 1 at threshold`) and the reason scores from
//! checks measured in metres, degrees and seconds can be compared and fused at all.
//!
//! # Why this is not one of the twelve
//!
//! The ported legacy suite has `headingInconsistency`, and it is a different check: it
//! compares the *claimed heading* against the *bearing of the claimed motion*. A sender
//! whose claimed heading spins 180° between two beacons while still pointing along its
//! own claimed displacement passes that check and fails this one. That is the gap this
//! example fills, and it is the kind of gap a real contributor arrives with.
//!
//! # The three rules this crate demonstrates that no other example does
//!
//! 1. **A detector reads belief, never ground truth.** Look at
//!    [`Detector::on_message`]'s arguments: a [`SelfBelief`], an [`ObservedMessage`] and a
//!    [`LocalEnvironment`]. There is no world, no actor id and no true position — not by
//!    convention but because the *argument types carry none*. A detector that wanted the
//!    truth could not ask for it (invariant I-T2).
//! 2. **Decisions read the node's own clock, not the simulator's.** Every threshold here
//!    is compared against [`SelfBelief::believed_time`] and the *claimed* generation
//!    times. `ThreatCtx::now()` is used for exactly one thing — the timestamp on the
//!    emitted record, which the recorder needs in simulator time so the channel joins
//!    with every other channel. Reading the host clock for a freshness decision is how a
//!    clock attack becomes invisible.
//! 3. **Quantise before a threshold comparison.** Build decision D10: a transcendental
//!    result must not drive a comparison whose outcome is compared across engines.
//!    Perturbing every transcendental by one unit in the last place moved one legacy
//!    configuration's report count from 3,082 to 3,135. So the score is rounded onto its
//!    1e-3 grid *before* it is compared with 1.0, and the number that reaches the record
//!    is the number the comparison used.
//!
//! # The interface limitation you will hit, stated here rather than discovered
//!
//! [`Verdict`] names its checks with [`DetectorId`], which is a **closed enumeration of
//! the ported legacy fifteen**. An out-of-tree detector therefore cannot name a new check:
//! it has to report under the nearest existing id, and this one reports under
//! [`DetectorId::HeadingInconsistency`]. That is honest — it *is* a heading inconsistency
//! — but it means two different checks share one column in the machine-learning
//! fingerprint, and a fusion model cannot tell them apart.
//!
//! This is a real gap in the seam, not a property of this example. Closing it means either
//! a `DetectorId::Plugin(PluginCheckId)` variant derived from the model id the way
//! `RngDomain::Plugin` already is, or a `Verdict` that carries `&'static str` check names
//! beside the enumeration. Until then, a contributor's detector is visible in
//! `det.observation` (which carries the detector name as a string and can therefore say
//! `example/detector/heading-rate`) and invisible in the fingerprint.
//!
//! # What it does not catch
//!
//! * A sender that changes heading slowly and consistently while lying about everything
//!   else. This check looks at one field's rate of change and nothing more.
//! * A first beacon from any signer: there is no previous heading, so there is no rate.
//!   A pseudonym change therefore resets the check, which is a genuine and unavoidable
//!   cost of unlinkability — and a reason an attacker changes pseudonym often.
//! * A stationary or slow sender, whose heading is not meaningfully defined. The check is
//!   gated on the claimed speed, with the legacy suite's own gate value.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde_json::json;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;
use v2xw_threat::{
    DetectorCost, DetectorId, Fingerprint, LocalEnvironment, Observation, ObservedKind,
    ObservedMessage, SelfBelief, ThreatCtx, ThreatCtxExt, Verdict,
};

/// The model's stable id, and the name that reaches the `det.observation` channel.
pub const MODEL_ID: &str = "example/detector/heading-rate";

/// The model's own version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The grid every score is rounded onto before it is compared with anything, and before
/// it is recorded (build decisions D9 and D10).
///
/// 1e-3, the legacy three-decimal convention, which is the grid the `detnorm_*` columns
/// of the machine-learning corpus already sit on.
pub const SCORE_QUANTUM: f64 = 1e-3;

/// Nanoseconds in a second. Named rather than written as `1e9` at three call sites.
const NS_PER_S: f64 = 1_000_000_000.0;

/// Every number the check reads. All four appear on the card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadingRateParams {
    /// The heading change that scores exactly 1.0, degrees.
    pub max_heading_change_deg: f64,
    /// The longest interval between two beacons that the check will still compare across,
    /// seconds. Beyond it the stored heading is too old to be a reference.
    pub max_time_delta_s: f64,
    /// The claimed speed below which the check is skipped, m/s.
    pub min_speed_mps: f64,
    /// How many consecutive scores at or above 1.0 from one signer before the check fires.
    pub min_consecutive: u32,
}

impl Default for HeadingRateParams {
    /// The cited defaults: F2MD's `MAX_HEADING_CHANGE` and `MAX_TIME_DELTA`, and the
    /// legacy suite's own heading-check speed gate and streak length.
    fn default() -> Self {
        Self {
            max_heading_change_deg: 90.0,
            max_time_delta_s: 3.1,
            min_speed_mps: 3.0,
            min_consecutive: 2,
        }
    }
}

/// What the check remembers about one signer.
///
/// Keyed by `HashedId8` in a [`BTreeMap`], never a `HashMap`: the map is iterated nowhere
/// today, but a `HashMap` here is one refactor away from putting a hash seed into an
/// output ordering, and the rule is cheaper to keep than to audit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Last {
    /// The heading the signer last claimed, radians.
    heading_rad: f64,
    /// The generation time it claimed for that beacon.
    claimed_at: SimTime,
    /// How many consecutive beacons from this signer have scored at or above 1.0.
    streak: u32,
}

/// The detector.
#[derive(Debug, Clone)]
pub struct HeadingRate {
    card: ModelCard,
    params: HeadingRateParams,
    /// One entry per signer this node has heard. A pseudonym change creates a new entry
    /// and the old one is never revisited, which is the check's cost under unlinkability.
    seen: BTreeMap<[u8; 8], Last>,
}

impl HeadingRate {
    /// A detector with `params`, having heard nothing.
    #[must_use]
    pub fn new(params: HeadingRateParams) -> Self {
        Self {
            card: card(&params),
            params,
            seen: BTreeMap::new(),
        }
    }

    /// A detector with the cited defaults.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(HeadingRateParams::default())
    }

    /// The parameters this instance reads.
    #[must_use]
    pub const fn params(&self) -> &HeadingRateParams {
        &self.params
    }

    /// How many signers it is tracking.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.seen.len()
    }

    /// The score for one beacon, without the record-keeping — the arithmetic, on its own.
    ///
    /// Returns the score already on [`SCORE_QUANTUM`], because the comparison against 1.0
    /// has to be made on the quantised value (D10) and returning the raw one would invite
    /// a caller to compare the wrong number.
    #[must_use]
    pub fn score(&self, previous_heading_rad: f64, heading_rad: f64) -> f64 {
        let change_deg = angle_difference_rad(heading_rad, previous_heading_rad).abs()
            * 180.0
            / core::f64::consts::PI;
        math::quantize_to(change_deg / self.params.max_heading_change_deg, SCORE_QUANTUM)
    }
}

/// The smallest signed difference between two angles, radians, in `(-pi, pi]`.
///
/// Written with `atan2` of the sine and cosine rather than by subtracting and wrapping,
/// because the wrap-around form is the classic place a heading check goes wrong at the
/// 0/2pi seam: a sender turning from 359° to 1° has changed by 2°, not 358°.
///
/// Both transcendentals come from [`v2xw_core::math`], never from the standard library.
#[must_use]
pub fn angle_difference_rad(a: f64, b: f64) -> f64 {
    let (sin_a, cos_a) = math::sin_cos(a);
    let (sin_b, cos_b) = math::sin_cos(b);
    // sin(a − b) and cos(a − b), from the addition formulae, so no subtraction of angles
    // happens before the wrap.
    math::atan2(
        sin_a * cos_b - cos_a * sin_b,
        cos_a * cos_b + sin_a * sin_b,
    )
}

impl Model for HeadingRate {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl v2xw_threat::Detector for HeadingRate {
    fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        me: &SelfBelief,
        m: &ObservedMessage,
        // This detector needs no map. The argument is in the signature because one of the
        // legacy twelve does (`mapOffRoad`), and a node without a map supplies `NoMap`,
        // whose honest answer is "every point is on the road" — which makes that check
        // score zero rather than quietly borrowing the world.
        _env: &dyn LocalEnvironment,
    ) -> Verdict {
        let subject = m.signer_hex();
        let mut fingerprint = Fingerprint::default();
        let mut fired = Vec::new();

        // `matches!` rather than a `match` with arms for every variant: `ObservedKind`
        // gains variants as the message layer grows (a collective-perception message, an
        // event message), and a detector that only looks at beacons should not have to be
        // edited every time one appears.
        let is_beacon = matches!(m.kind, ObservedKind::Beacon);
        // A content check on an unverified or badly signed message is worthless: the
        // claim is not attributable to the signer at all, so a score against it would be
        // an accusation against whoever's digest was on the envelope. The legacy suite
        // zeroes every plausibility check on a bad signature for exactly this reason.
        let attributable = m.verification.is_valid();
        let fast_enough = m.claimed_speed_mps >= self.params.min_speed_mps;

        if !(is_beacon && attributable && fast_enough) {
            return Verdict {
                subject,
                fingerprint,
                fired,
            };
        }

        let previous = self.seen.get(&m.signer).copied();
        let mut streak = 0;
        if let Some(last) = previous {
            // The interval the two beacons *claim* to span, from claimed generation times:
            // the sender's own timeline, which is what a heading rate is a rate over. A
            // negative interval means the claims are out of order, which is a different
            // check's business (`staleOrReplay`), so this one abstains and re-anchors.
            let delta_s = (m.claimed_generation_time as f64 - last.claimed_at as f64) / NS_PER_S;
            if delta_s > 0.0 && delta_s <= self.params.max_time_delta_s {
                let score = self.score(last.heading_rad, m.claimed_heading_rad);
                fingerprint.set(DetectorId::HeadingInconsistency, score);
                // The comparison is on the quantised score, per D10.
                if score >= 1.0 {
                    streak = last.streak.saturating_add(1);
                    if streak >= self.params.min_consecutive {
                        let observation = Observation {
                            detector: DetectorId::HeadingInconsistency,
                            score,
                            subject: subject.clone(),
                            // The observing node's own clock. Not `ctx.now()`.
                            at: me.believed_time,
                        };
                        fired.push(observation);
                        // One record per firing, on the node-visible channel. `t` is the
                        // host's simulated instant, which is the only thing `ctx.now()` is
                        // for: the recorder needs every channel on one timeline.
                        //
                        // Read into a local first. `ctx.emit(X { t: ctx.now(), .. })` needs
                        // a shared borrow inside the argument of a call that takes a
                        // mutable one, which two-phase borrows probably allow and which is
                        // not worth depending on in a file people copy.
                        let host_now = ctx.now();
                        ctx.emit(v2xw_threat::DetObservation {
                            t: host_now,
                            node: me.node,
                            detector: MODEL_ID.to_string(),
                            subject: subject.clone(),
                            score: Some(score),
                        });
                    }
                }
            }
        }

        self.seen.insert(
            m.signer,
            Last {
                heading_rad: m.claimed_heading_rad,
                claimed_at: m.claimed_generation_time,
                streak,
            },
        );

        Verdict {
            subject,
            fingerprint,
            fired,
        }
    }

    fn cost(&self) -> DetectorCost {
        // Four transcendentals, two floating-point divisions and one B-tree lookup. The
        // figure is a **declared guess**: nothing has measured it, and `DetectorCost` has
        // no place to say so, which is why it is said here and on the card.
        //
        // It matters more than it looks: the node runtime charges this against the
        // profile's CPU budget, so a wrong number changes how many messages a saturated
        // node gets through. The calibration plan is on the card's `per_message_us`
        // parameter.
        DetectorCost {
            per_message_us: 1.0,
        }
    }
}

/// Builds the card.
#[must_use]
pub fn card(params: &HeadingRateParams) -> ModelCard {
    let f2md = Source {
        kind: SourceKind::Code,
        reference: "veins-F2MD `F2MDParameters.h` (MAX_HEADING_CHANGE 90 deg, \
                    MAX_TIME_DELTA 3.1 s), via 04-models.md §14"
            .to_string(),
        accessed: Some("2026-09-22".to_string()),
        note: Some(
            "In F2MD the 90 deg bound limits the disagreement between a claimed heading \
             and the heading implied by the claimed positions (PositionHeadingConsistancy). \
             Reusing it as a bound on the heading *change* between two beacons is a design \
             choice recorded here: the value is cited, its application is not. See the \
             card's limitations."
                .to_string(),
        ),
    };
    let legacy = Source {
        kind: SourceKind::Code,
        reference: "legacy/scms_sim_ref/mock_pipeline/run.py — the heading check's speed \
                    gate (`cs > 3.0`) and `PipelineConfig.detector_min_consec` (2), as \
                    ported in `v2xw_threat::DetectorParams`"
            .to_string(),
        accessed: Some("2026-09-22".to_string()),
        note: Some(
            "The streak gate is what separates one GNSS outlier from a sustained attack; \
             the speed gate is there because a stationary vehicle's claimed heading is not \
             meaningfully defined."
                .to_string(),
        ),
    };

    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Detector,
        MODEL_VERSION,
        "Worked example. Scores the rate of change of a signer's claimed heading against \
         what a vehicle can physically do, from the receiver's own belief only. \
         Complements the ported legacy `headingInconsistency`, which compares the claimed \
         heading against the bearing of the claimed motion and therefore passes a sender \
         whose heading spins while its displacement stays consistent.",
    );
    card.tier = vec![Tier::Medium, Tier::High];

    card.equations = vec![
        Equation {
            name: "score".to_string(),
            latex_or_text: "score = |angle(h_now − h_prev)| / max_heading_change".to_string(),
            notes: Some(
                "Normalised so that 1.0 is the firing threshold, which is the `detnorm` \
                 contract every detector here keeps. The angle difference is taken through \
                 atan2 of the sine and cosine, so the 0/2pi seam cannot turn a 2 deg turn \
                 into a 358 deg one."
                    .to_string(),
            ),
        },
        Equation {
            name: "firing rule".to_string(),
            latex_or_text: "fire when score >= 1.0 on min_consecutive beacons in a row \
                            from one signer"
                .to_string(),
            notes: Some(
                "The score is quantised onto 1e-3 before the comparison (build decision \
                 D10), so the fired set is a function of the recorded number."
                    .to_string(),
            ),
        },
    ];

    card.parameters = vec![
        Parameter {
            range: Some(vec![json!(1.0), json!(180.0)]),
            ..Parameter::new(
                "max_heading_change_deg",
                "deg",
                json!(params.max_heading_change_deg),
                f2md.clone(),
            )
        },
        Parameter {
            range: Some(vec![json!(0.1), json!(30.0)]),
            ..Parameter::new(
                "max_time_delta_s",
                "s",
                json!(params.max_time_delta_s),
                f2md,
            )
        },
        Parameter {
            range: Some(vec![json!(0.0), json!(20.0)]),
            ..Parameter::new(
                "min_speed_mps",
                "m/s",
                json!(params.min_speed_mps),
                legacy.clone(),
            )
        },
        Parameter {
            range: Some(vec![json!(1), json!(10)]),
            ..Parameter::new(
                "min_consecutive",
                "-",
                json!(params.min_consecutive),
                legacy,
            )
        },
        // The CPU cost this detector charges the node. It is a parameter and not a
        // constant precisely because it is uncalibrated: the node runtime spends it out of
        // the hardware profile's budget, so it changes results.
        Parameter {
            range: Some(vec![json!(0.0), json!(1000.0)]),
            calibration: Some(
                "Plan: benchmark `Detector::on_message` on the reference laptop over the \
                 1,330-message claim trace `v2xw-threat`'s legacy comparison test already \
                 holds, divide by the message count, and record the figure with the \
                 machine it was measured on. Until then this is an implementer's guess and \
                 a saturated-node result is sensitive to it."
                    .to_string(),
            ),
            ..Parameter::new(
                "per_message_us",
                "us",
                json!(1.0),
                Source::todo_calibrate("the CPU cost of one check, never measured"),
            )
        },
    ];

    card.assumptions = vec![
        "Two consecutive beacons from one signer are the same physical sender. A Sybil \
         that rotates pseudonyms per beacon defeats this by construction, which is what \
         `sybilCoLocation` is for."
            .to_string(),
        "The claimed generation times order the sender's own beacons. An out-of-order \
         claim makes this check abstain rather than guess."
            .to_string(),
    ];

    card.limitations = vec![
        "The 90 deg bound is cited to F2MD, where it bounds a different quantity. Treat it \
         as an order of magnitude, not as a measured yaw-rate limit; a study of turning \
         behaviour should fit it against the world's own junction geometry."
            .to_string(),
        "The bound does not scale with the interval, so a change over 3 s and a change \
         over 0.1 s score the same. A rate form would divide by the interval; that is a \
         better check and needs a cited maximum yaw rate, which is why it is not here."
            .to_string(),
        "It reports under `DetectorId::HeadingInconsistency`, sharing a fingerprint column \
         with a different check — see the crate documentation."
            .to_string(),
        "`per_message_us` is uncalibrated.".to_string(),
    ];

    card.ignores = vec![
        "Everything but one field's rate of change. This is a single-feature check and is \
         meant to be fused, not used alone."
            .to_string(),
        "The claimed position, so a heading that disagrees with the displacement is \
         invisible here (that is `headingInconsistency`)."
            .to_string(),
        "Anything about a signer's first beacon, and therefore the whole interval after a \
         pseudonym change."
            .to_string(),
    ];

    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "a_reversed_heading_fires_after_the_streak".to_string(),
            "the_zero_two_pi_seam_is_not_a_turn".to_string(),
            "a_slow_sender_is_not_checked".to_string(),
            "an_unverified_message_is_not_scored".to_string(),
            "the_conformance_suite_passes".to_string(),
        ],
    };

    // Nothing is drawn. A detector that wanted randomness — a sampled check, a randomised
    // response — would declare its domain here and draw through `ctx.rng`.
    card.determinism = Determinism::default();

    card
}

/// Registers the model.
///
/// # Errors
/// Whatever the registry refused, by name.
pub fn register(
    registry: &mut v2xw_core::registry::Registry,
) -> Result<v2xw_core::registry::ModelRef, v2xw_core::registry::RegistryError> {
    let model: v2xw_core::model::ModelHandle = std::sync::Arc::new(HeadingRate::with_defaults());
    registry.register_model(model)
}

/// One beacon, as a receiver observed it.
///
/// A helper rather than a struct literal at every call site, so that a field added to
/// [`ObservedMessage`] upstream is one edit here and not twenty in the tests. Public
/// because the conformance subject below uses it and a contributor's own tests will too.
#[must_use]
pub fn beacon(
    signer: [u8; 8],
    claimed_generation_time: SimTime,
    heading_rad: f64,
    speed_mps: f64,
) -> ObservedMessage {
    ObservedMessage {
        signer,
        kind: ObservedKind::Beacon,
        received_at: claimed_generation_time,
        claimed_generation_time,
        claimed_x_m: 0.0,
        claimed_y_m: 0.0,
        claimed_speed_mps: speed_mps,
        claimed_heading_rad: heading_rad,
        claimed_pos_confidence_m: 2.0,
        repetitions: 1,
        cert_valid_from: 0,
        cert_valid_to: SimTime::MAX,
        station_type: v2xw_threat::StationType::Vehicle,
        verification: v2xw_threat::VerificationState::Valid,
    }
}

/// What a receiver believes about itself, for a test or a conformance probe.
#[must_use]
pub fn belief(node: u32, believed_time: SimTime) -> SelfBelief {
    SelfBelief {
        node: v2xw_core::ids::NodeId::new(node),
        believed_time,
        x_m: 0.0,
        y_m: 0.0,
        radio_range_m: 500.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::registry::Registry;
    use v2xw_threat::{CollectingCtx, Detector, NoMap};

    const SIGNER: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const OTHER: [u8; 8] = [9, 9, 9, 9, 9, 9, 9, 9];

    /// Ten hertz, in nanoseconds: the BSM generation interval.
    const STEP_NS: SimTime = 100_000_000;

    fn run(
        detector: &mut HeadingRate,
        ctx: &mut CollectingCtx,
        signer: [u8; 8],
        headings_rad: &[f64],
        speed_mps: f64,
    ) -> Vec<Verdict> {
        let mut out = Vec::new();
        for (i, heading) in headings_rad.iter().enumerate() {
            let t = (i as SimTime + 1) * STEP_NS;
            ctx.set_now(t);
            out.push(detector.on_message(
                &mut *ctx,
                &belief(0, t),
                &beacon(signer, t, *heading, speed_mps),
                &NoMap,
            ));
        }
        out
    }

    #[test]
    fn the_card_validates() {
        card(&HeadingRateParams::default())
            .validate()
            .expect("the card must validate");
    }

    #[test]
    fn the_card_registers_and_the_registry_refuses_it_twice() {
        let mut registry = Registry::new();
        register(&mut registry).expect("the first registration must succeed");
        assert!(
            register(&mut registry).is_err(),
            "a duplicate id must be refused"
        );
    }

    /// The assertion the whole model exists for.
    ///
    /// **Shown to fail:** raising `max_heading_change_deg` to 200 makes the score fall
    /// below 1.0 and this test goes red, which is how it was checked. A test that fired on
    /// any input at all would be measuring the plumbing and not the check.
    #[test]
    fn a_reversed_heading_fires_after_the_streak() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        // 0, pi, 0: two consecutive 180 deg reversals, which is a score of 2.0 twice.
        let verdicts = run(
            &mut d,
            &mut ctx,
            SIGNER,
            &[0.0, core::f64::consts::PI, 0.0],
            20.0,
        );
        assert!(!verdicts[0].fired(), "the first beacon has no reference");
        assert!(
            !verdicts[1].fired(),
            "one violation is not a streak: min_consecutive is 2"
        );
        assert!(verdicts[2].fired(), "the second consecutive violation fires");
        let leading = verdicts[2].leading().expect("a fired verdict has a leader");
        assert_eq!(leading.detector, DetectorId::HeadingInconsistency);
        assert!((leading.score - 2.0).abs() < 1e-9, "got {}", leading.score);
        // …and the score is in the fingerprint on every scored beacon, fired or not.
        assert!((verdicts[1].fingerprint.get(DetectorId::HeadingInconsistency) - 2.0).abs() < 1e-9);
        // One record, on the node-visible channel, only for the firing.
        assert_eq!(ctx.on_channel("det.observation").len(), 1);
    }

    #[test]
    fn a_gentle_turn_does_not_fire() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        // 10 deg per beacon for a second: a normal turn through a junction.
        let step = 10.0_f64.to_radians();
        let headings: Vec<f64> = (0..10).map(|i| f64::from(i) * step).collect();
        let verdicts = run(&mut d, &mut ctx, SIGNER, &headings, 12.0);
        assert!(verdicts.iter().all(|v| !v.fired()));
        assert!(ctx.on_channel("det.observation").is_empty());
    }

    /// The seam that breaks naive heading arithmetic.
    #[test]
    fn the_zero_two_pi_seam_is_not_a_turn() {
        let d = HeadingRate::with_defaults();
        let two_pi = core::f64::consts::TAU;
        // 359 deg to 1 deg is a 2 deg turn.
        let score = d.score(359.0_f64.to_radians(), 1.0_f64.to_radians());
        assert!(score < 0.05, "expected a small score, got {score}");
        // And the same heading expressed one revolution apart is no turn at all.
        assert_eq!(d.score(0.5, 0.5 + two_pi), 0.0);
    }

    #[test]
    fn a_slow_sender_is_not_checked() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        // The same reversals as the firing test, at 1 m/s: below the speed gate, so
        // nothing is scored and nothing is remembered.
        let verdicts = run(
            &mut d,
            &mut ctx,
            SIGNER,
            &[0.0, core::f64::consts::PI, 0.0],
            1.0,
        );
        assert!(verdicts.iter().all(|v| !v.fired()));
        assert_eq!(d.tracked(), 0, "a gated-out beacon must not become a reference");
    }

    #[test]
    fn an_unverified_message_is_not_scored() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        let mut first = beacon(SIGNER, STEP_NS, 0.0, 20.0);
        first.verification = v2xw_threat::VerificationState::Unverified;
        let verdict = d.on_message(&mut ctx, &belief(0, STEP_NS), &first, &NoMap);
        assert!(!verdict.fired());
        assert_eq!(
            verdict.fingerprint.get(DetectorId::HeadingInconsistency),
            0.0
        );
        assert_eq!(d.tracked(), 0);
    }

    #[test]
    fn a_stale_reference_is_not_compared_against() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        // Two reversals ten seconds apart: further than max_time_delta_s, so the check
        // re-anchors instead of accusing. A vehicle can turn round in ten seconds.
        let far = 10 * 1_000_000_000;
        d.on_message(
            &mut ctx,
            &belief(0, 1),
            &beacon(SIGNER, 1, 0.0, 20.0),
            &NoMap,
        );
        let verdict = d.on_message(
            &mut ctx,
            &belief(0, far),
            &beacon(SIGNER, far, core::f64::consts::PI, 20.0),
            &NoMap,
        );
        assert_eq!(
            verdict.fingerprint.get(DetectorId::HeadingInconsistency),
            0.0,
            "a reference older than max_time_delta_s must not be scored against"
        );
    }

    #[test]
    fn two_signers_do_not_share_a_streak() {
        let mut d = HeadingRate::with_defaults();
        let mut ctx = CollectingCtx::new(1);
        // Alternating signers, each reversing every time it speaks. If the streak were
        // kept per node rather than per signer, the third beacon would fire.
        let pi = core::f64::consts::PI;
        let script = [(SIGNER, 0.0), (OTHER, 0.0), (SIGNER, pi), (OTHER, pi)];
        let mut fired = 0;
        for (i, (signer, heading)) in script.iter().enumerate() {
            let t = (i as SimTime + 1) * STEP_NS;
            ctx.set_now(t);
            if d.on_message(
                &mut ctx,
                &belief(0, t),
                &beacon(*signer, t, *heading, 20.0),
                &NoMap,
            )
            .fired()
            {
                fired += 1;
            }
        }
        assert_eq!(fired, 0, "each signer has one violation, not two");
        assert_eq!(d.tracked(), 2);
    }

    #[test]
    fn every_score_sits_on_its_grid() {
        // Build decision D9, checked on the number that actually leaves the model.
        let d = HeadingRate::with_defaults();
        let mut heading = 0.0;
        while heading < core::f64::consts::TAU {
            let score = d.score(0.0, heading);
            assert!(
                v2xw_core::math::is_on_grid(score, SCORE_QUANTUM),
                "{score} is off the {SCORE_QUANTUM} grid"
            );
            heading += 0.017;
        }
    }
}

/// The plug-in conformance suite (03-interfaces.md §17), wired up.
///
/// This module is the thing to copy. A contributor's own crate implements
/// [`v2xw_conformance::plugin::PluginUnderTest`] the same way, runs `cargo test`, and gets
/// the five properties §17 requires checked against their model rather than against a
/// promise: no owned generator, no wall clock, no ground truth, a valid card with a source
/// for every default, and identical values forwards, backwards and across eight threads.
///
/// Read [`Subject::exercise`] first. Everything else is bookkeeping.
#[cfg(test)]
mod conformance {
    use super::*;
    use v2xw_conformance::plugin::{PluginUnderTest, ProbeCtx, run_suite};
    use v2xw_core::ids::NodeId;
    use v2xw_core::rng::EntityRef;
    use v2xw_threat::Detector;

    /// The model, as the suite drives it.
    ///
    /// It holds the *card* and not the detector, and builds a fresh detector inside
    /// [`Subject::exercise`]. That is deliberate and it is the only shape that works:
    /// `exercise` takes `&self` (the suite runs it on eight threads at once), while
    /// `Detector::on_message` takes `&mut self` because the check keeps per-signer
    /// history. Building one per exercise also makes the reordering property *mean*
    /// something — if the check leaked state between entities, the fresh instance would
    /// hide it, so the state under test is the state inside one call sequence.
    struct Subject {
        model: HeadingRate,
    }

    impl Subject {
        fn new() -> Self {
            Self {
                model: HeadingRate::with_defaults(),
            }
        }
    }

    impl PluginUnderTest for Subject {
        fn model(&self) -> &dyn Model {
            &self.model
        }

        /// Two entities, so the reordering and thread-independence checks have something
        /// to compare. A detector acts for the node it runs on, so the entity is a node.
        fn entities(&self) -> Vec<EntityRef> {
            vec![
                EntityRef::Node(NodeId::new(1)),
                EntityRef::Node(NodeId::new(2)),
            ]
        }

        /// One deterministic unit of work: a scripted beacon sequence from one sender,
        /// scored, with the scores returned.
        ///
        /// The heading step is derived from the entity, so the two entities produce
        /// different numbers and the per-entity comparison is not comparing two copies of
        /// the same vector.
        fn exercise(&self, ctx: &mut ProbeCtx, entity: EntityRef) -> Vec<f64> {
            let node = match entity {
                EntityRef::Node(n) => n.index(),
                _ => 0,
            };
            let mut detector = HeadingRate::with_defaults();
            // A turn that grows with the node index: node 1 turns 50 deg per beacon and
            // scores 0.556, node 2 turns 100 deg and scores 1.111, so one of them crosses
            // the 90 deg bound and the other does not. The suite is therefore comparing
            // two genuinely different answers — and the firing side also emits a record,
            // which is what gives the recorder-discipline check something to inspect.
            let step = f64::from(node) * 50.0_f64.to_radians();
            let mut out = Vec::new();
            for i in 0..6u64 {
                let t = (i + 1) * 100_000_000;
                ctx.set_now(t);
                let heading = (i as f64) * step;
                let verdict = detector.on_message(
                    &mut *ctx,
                    &belief(node, t),
                    &beacon([node as u8; 8], t, heading, 20.0),
                    &v2xw_threat::NoMap,
                );
                out.push(verdict.fingerprint.get(DetectorId::HeadingInconsistency));
            }
            out
        }

        /// The suite scans these for an owned generator, a wall-clock read and a reach at
        /// ground truth. A plug-in that names no source files gets no scan and the report
        /// says `----` rather than `pass`, which is the honest answer — so name them.
        fn sources(&self) -> Vec<std::path::PathBuf> {
            vec![std::path::PathBuf::from(file!())]
        }

        /// Every value [`Subject::exercise`] returns is a `detnorm` score, whose grid is
        /// [`SCORE_QUANTUM`].
        fn quantum(&self) -> f64 {
            SCORE_QUANTUM
        }

        /// A detector runs inside a node, so no record it emits may carry ground truth.
        /// Saying so turns invariant I-C2 into a check on the records this model actually
        /// wrote.
        fn runs_as_node(&self) -> bool {
            true
        }
    }

    #[test]
    fn the_conformance_suite_passes() {
        let subject = Subject::new();
        let report = run_suite(&subject, 0x5EED);
        // `passed()` tolerates a check the suite could not run; `complete()` does not.
        // This model names its source file and declares its grid, so every check runs.
        assert!(report.complete(), "{report}");
    }

    #[test]
    fn the_suite_is_a_function_of_the_seed() {
        let subject = Subject::new();
        let a = run_suite(&subject, 7);
        let b = run_suite(&subject, 7);
        assert_eq!(a.checks.len(), b.checks.len());
        for (x, y) in a.checks.iter().zip(b.checks.iter()) {
            assert_eq!(x.name, y.name);
            assert_eq!(x.verdict, y.verdict);
        }
    }
}
