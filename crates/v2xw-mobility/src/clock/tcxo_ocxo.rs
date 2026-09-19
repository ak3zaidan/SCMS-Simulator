//! `clock/drift/tcxo-ocxo` — oscillator drift in holdover (04-models.md §3.8).
//!
//! # The rule
//!
//! While the fix is a satellite fix, node time *is* GNSS time, to within the time-transfer
//! error the GPS SPS performance standard commits to: **≤ 30 ns at 95 %**
//! [GPS SPS PS 2020, R3 §G.7]. The moment the fix is lost the clock free-runs at its
//! oscillator's fractional frequency error, so the offset grows linearly with the holdover
//! interval:
//!
//! ```text
//! offset(t) = offset(t_lost) + ε · (t − t_lost)      ε in parts per unit (1 ppm = 1 µs/s)
//! ```
//!
//! # The two oscillators
//!
//! | Class | Fractional error | Source |
//! |---|---|---|
//! | Automotive TCXO | ±0.5 to ±5.0 ppm over −40…+85 °C (grade G3) or +105 °C (G2) | TXC TCXO page [R3 §G.7] |
//! | PPS-disciplined OCXO | ≤ 0.5 ppb (some ≤ 0.1 ppb), ≤ 1.5 µs of phase error over 24-48 h holdover | Rakon PPS-OCXO page [R3 §G.7] |
//!
//! §3.8's defaults: an OBU carries a TCXO at **2 ppm** (the mid-range of the cited band,
//! and §3.8 records that choosing the mid-range is a design choice) and an RSU an OCXO at
//! **0.5 ppb**. 1PPS discipline is standard on commercial OBUs [Unex OBU-301E, Autotalks
//! CRATON, R3 §G.7].
//!
//! # Why the sign is drawn once per node
//!
//! A fractional frequency error is a property of a crystal, not of an instant: a given unit
//! runs consistently fast or consistently slow. So each node draws its error once, from its
//! own stream, uniformly in `[−ε_max, +ε_max]`, and keeps it. A model that redrew per step
//! would produce a random walk instead of a drift, and its holdover error would grow as
//! `√t` instead of `t` — which is the wrong physics and, worse, a plausible-looking wrong
//! answer.
//!
//! # Its own RNG domain
//!
//! The draws come from a *plug-in* domain derived from this model's id
//! ([`v2xw_core::rng::RngDomain::plugin`]), not from [`v2xw_core::rng::RngDomain::Gnss`].
//! Sharing the GNSS domain and entity would put two models on one stream, and then the
//! clock's draw would depend on how many times the GNSS model had drawn first — exactly the
//! order dependence ADR 0004 §3 exists to remove.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::SimTime;
use v2xw_core::{FixQuality, time::ns_to_secs, time::secs_to_ns};

use crate::ctx::MobCtx;
use crate::traits::ClockModel;

/// The model id.
pub const MODEL_ID: &str = "clock/drift/tcxo-ocxo";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The GPS SPS committed 95 % time-transfer error, nanoseconds [GPS SPS PS 2020].
pub const SPS_TIME_TRANSFER_P95_NS: f64 = 30.0;

/// Which oscillator a node carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Oscillator {
    /// An automotive TCXO: the OBU default, 2 ppm.
    #[default]
    Tcxo,
    /// A PPS-disciplined OCXO: the RSU default, 0.5 ppb.
    Ocxo,
}

impl Oscillator {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Oscillator::Tcxo => "tcxo",
            Oscillator::Ocxo => "ocxo",
        }
    }

    /// The default fractional frequency error, dimensionless (1 ppm = 1e-6).
    ///
    /// TCXO 2 ppm — the mid-range of the cited ±0.5 to ±5.0 ppm band, which §3.8 records as
    /// a design choice — and OCXO 0.5 ppb.
    pub const fn default_fractional_error(self) -> f64 {
        match self {
            Oscillator::Tcxo => 2e-6,
            Oscillator::Ocxo => 0.5e-9,
        }
    }

    /// The cited band for this class, `(min, max)`, dimensionless.
    pub const fn cited_band(self) -> (f64, f64) {
        match self {
            Oscillator::Tcxo => (0.5e-6, 5.0e-6),
            // "≤ 0.5 ppb (some ≤ 0.1 ppb)".
            Oscillator::Ocxo => (0.1e-9, 0.5e-9),
        }
    }
}

/// The clock's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OscillatorParams {
    /// Which oscillator.
    pub oscillator: Oscillator,
    /// The magnitude of the fractional frequency error, dimensionless. A node's own error
    /// is drawn uniformly in `[−this, +this]` and kept.
    pub fractional_error: f64,
    /// Whether the node is 1PPS-disciplined, i.e. whether a valid fix resets the offset.
    pub pps_disciplined: bool,
    /// The standard deviation of the time-transfer error while disciplined, nanoseconds.
    ///
    /// DERIVED from the committed 95 % bound: `σ = 30 ns / 1.96`.
    pub transfer_sigma_ns: f64,
}

impl Default for OscillatorParams {
    /// The OBU default of §3.8: a TCXO at 2 ppm, 1PPS-disciplined.
    fn default() -> Self {
        Self::for_oscillator(Oscillator::Tcxo)
    }
}

impl OscillatorParams {
    /// The defaults for one oscillator class.
    pub fn for_oscillator(oscillator: Oscillator) -> Self {
        Self {
            oscillator,
            fractional_error: oscillator.default_fractional_error(),
            pps_disciplined: true,
            transfer_sigma_ns: SPS_TIME_TRANSFER_P95_NS / 1.959963984540054,
        }
    }

    /// The RSU default: an OCXO at 0.5 ppb.
    pub fn rsu() -> Self {
        Self::for_oscillator(Oscillator::Ocxo)
    }
}

/// One node's clock state.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NodeClock {
    /// This unit's own fractional frequency error, drawn once.
    epsilon: f64,
    /// The believed-minus-true offset, seconds.
    offset_s: f64,
    /// When the clock was last read.
    last: SimTime,
}

/// The oscillator clock.
#[derive(Debug, Clone)]
pub struct OscillatorClock {
    params: OscillatorParams,
    state: BTreeMap<NodeId, NodeClock>,
    card: ModelCard,
}

impl Default for OscillatorClock {
    fn default() -> Self {
        OscillatorClock::new(OscillatorParams::default())
    }
}

impl OscillatorClock {
    /// The clock with the given parameters.
    pub fn new(params: OscillatorParams) -> Self {
        Self {
            card: card(&params),
            params,
            state: BTreeMap::new(),
        }
    }

    /// An OBU clock: a TCXO at 2 ppm.
    pub fn obu() -> Self {
        Self::new(OscillatorParams::default())
    }

    /// An RSU clock: an OCXO at 0.5 ppb.
    pub fn rsu() -> Self {
        Self::new(OscillatorParams::rsu())
    }

    /// The parameters in force.
    pub fn params(&self) -> &OscillatorParams {
        &self.params
    }

    /// One node's current believed-minus-true offset, seconds.
    pub fn offset_s(&self, node: NodeId) -> Option<f64> {
        self.state.get(&node).map(|c| c.offset_s)
    }

    /// One node's own fractional frequency error, once it has been drawn.
    pub fn fractional_error_of(&self, node: NodeId) -> Option<f64> {
        self.state.get(&node).map(|c| c.epsilon)
    }

    /// The offset a free-running clock of this class accumulates over `holdover_s`,
    /// seconds — the worst case, at the full magnitude.
    pub fn worst_case_offset_s(&self, holdover_s: f64) -> f64 {
        self.params.fractional_error * holdover_s
    }
}

impl v2xw_core::model::Model for OscillatorClock {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl ClockModel for OscillatorClock {
    fn read(&mut self, ctx: &mut dyn MobCtx, node: NodeId, gnss: &FixQuality) -> SimTime {
        let now = ctx.now();
        if !self.state.contains_key(&node) {
            // This unit's own frequency error, drawn once, from this model's own domain.
            let epsilon = ctx
                .rng(RngDomain::plugin(MODEL_ID), EntityRef::Node(node))
                .uniform(-self.params.fractional_error, self.params.fractional_error);
            self.state.insert(
                node,
                NodeClock {
                    epsilon,
                    offset_s: 0.0,
                    last: now,
                },
            );
        }
        let disciplined = self.params.pps_disciplined && gnss.is_satellite_fix();
        let transfer_ns = if disciplined {
            ctx.rng(RngDomain::plugin(MODEL_ID), EntityRef::Node(node))
                .normal(0.0, self.params.transfer_sigma_ns)
        } else {
            0.0
        };
        let clock = self.state.get_mut(&node).expect("inserted above");
        let dt_s = ns_to_secs(now.saturating_sub(clock.last));
        clock.last = now;
        if disciplined {
            // A valid satellite fix resets the offset to the time-transfer error.
            clock.offset_s = transfer_ns * 1e-9;
        } else {
            // Holdover: free-run at this unit's own fractional error.
            clock.offset_s += clock.epsilon * dt_s;
        }
        let offset_ns = secs_to_ns(clock.offset_s.abs());
        if clock.offset_s >= 0.0 {
            now.saturating_add(offset_ns)
        } else {
            now.saturating_sub(offset_ns)
        }
    }
}

/// The model card.
pub fn card(params: &OscillatorParams) -> ModelCard {
    let txc = Source {
        kind: SourceKind::Datasheet,
        reference: "TXC automotive TCXO: ±0.5 to ±5.0 ppm over −40…+85 °C (G3) or +105 °C \
                    (G2) [R3 §G.7]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: Some(
            "§3.8's default takes 2 ppm, the mid-range of the band; taking the mid-range is \
             a design choice §3.8 records"
                .to_string(),
        ),
    };
    let rakon = Source {
        kind: SourceKind::Datasheet,
        reference: "Rakon PPS-disciplined OCXO: ≤ 0.5 ppb (some ≤ 0.1 ppb), ≤ 1.5 µs phase \
                    error over 24-48 h holdover [R3 §G.7]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let sps = Source {
        kind: SourceKind::Standard,
        reference: "GPS SPS PS 2020 Table 3.8-3: time transfer ≤ 30 ns at 95 % [R3 §G.1]"
            .to_string(),
        accessed: Some("2026-09-17".to_string()),
        note: None,
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Clock,
        MODEL_VERSION,
        "A node's believed time: GNSS time while a satellite fix is valid, to within the \
         committed 30 ns time-transfer error, and a free run at the oscillator's own \
         fractional frequency error in holdover. The believed time feeds `generationTime` \
         and the plausibility windows of 04-models.md §14.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "disciplined".to_string(),
            latex_or_text: "believed = true + N(0, σ_transfer),  σ_transfer = 30 ns / 1.96"
                .to_string(),
            notes: Some("while the fix is a satellite fix".to_string()),
        },
        Equation {
            name: "holdover".to_string(),
            latex_or_text: "offset(t) = offset(t_lost) + ε·(t − t_lost)".to_string(),
            notes: Some(
                "ε is this unit's own fractional frequency error, drawn once and kept: a \
                 crystal runs consistently fast or slow, so the error grows as t and not \
                 as √t"
                    .to_string(),
            ),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "oscillator",
            "-",
            serde_json::json!(params.oscillator.label()),
            if params.oscillator == Oscillator::Ocxo {
                rakon.clone()
            } else {
                txc.clone()
            },
        ),
        Parameter::new(
            "fractional_error",
            "1",
            serde_json::json!(params.fractional_error),
            if params.oscillator == Oscillator::Ocxo {
                rakon.clone()
            } else {
                txc.clone()
            },
        ),
        Parameter::new(
            "cited_band",
            "1",
            serde_json::json!({
                "tcxo": [Oscillator::Tcxo.cited_band().0, Oscillator::Tcxo.cited_band().1],
                "ocxo": [Oscillator::Ocxo.cited_band().0, Oscillator::Ocxo.cited_band().1],
            }),
            txc.clone(),
        ),
        Parameter::new(
            "pps_disciplined",
            "-",
            serde_json::json!(params.pps_disciplined),
            Source {
                kind: SourceKind::Datasheet,
                reference: "1PPS discipline is standard on commercial OBUs [Unex OBU-301E, \
                            Autotalks CRATON, R3 §G.7]"
                    .to_string(),
                accessed: Some("2026-09-17".to_string()),
                note: None,
            },
        ),
        Parameter::new(
            "transfer_sigma",
            "ns",
            serde_json::json!(params.transfer_sigma_ns),
            sps.clone(),
        ),
        Parameter::new(
            "ocxo_holdover_phase_error",
            "µs",
            serde_json::json!(1.5),
            rakon.clone(),
        ),
    ];
    card.assumptions = vec![
        "A unit's fractional frequency error is drawn once and kept, so holdover error \
         grows linearly with the holdover interval."
            .to_string(),
        "The draws come from this model's own plug-in RNG domain, so they cannot depend on \
         how often the GNSS model has drawn for the same node."
            .to_string(),
        "Only a satellite fix disciplines the clock: a node in dead reckoning is a node \
         whose clock is free-running (`FixQuality::is_satellite_fix`)."
            .to_string(),
    ];
    card.limitations = vec![
        "No temperature model, no ageing, no Allan-variance structure: the drift is a \
         constant fractional error, which is what the cited datasheets bound."
            .to_string(),
    ];
    card.ignores = vec![
        "Short-term instability (phase noise, Allan deviation) and the temperature \
         dependence the cited band brackets."
            .to_string(),
    ];
    card.sources = vec![txc, rakon, sps];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::plugin(MODEL_ID).as_str().to_string()],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "clock::tcxo_ocxo::tests::holdover_drifts_at_the_cited_rate".to_string(),
            "clock::tcxo_ocxo::tests::a_valid_fix_disciplines_the_clock".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::MobilityCtx;
    use crate::worlds::{RingParams, ring};
    use v2xw_core::model::Model;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_S;
    use v2xw_world::World;

    fn world() -> World {
        ring(&RingParams::default()).expect("a ring")
    }

    #[test]
    fn the_cited_bands_and_defaults_are_the_document_values() {
        assert_eq!(Oscillator::Tcxo.default_fractional_error(), 2e-6);
        assert_eq!(Oscillator::Ocxo.default_fractional_error(), 0.5e-9);
        assert_eq!(Oscillator::Tcxo.cited_band(), (0.5e-6, 5.0e-6));
        assert_eq!(Oscillator::Ocxo.cited_band(), (0.1e-9, 0.5e-9));
        assert_eq!(SPS_TIME_TRANSFER_P95_NS, 30.0);
        // 2 ppm is inside the cited band, and is its mid-range.
        let (lo, hi) = Oscillator::Tcxo.cited_band();
        assert!(
            (0.5 * (lo + hi) - 2.75e-6).abs() < 1e-12,
            "the arithmetic mid-range"
        );
        assert!(Oscillator::Tcxo.default_fractional_error() >= lo);
        assert!(Oscillator::Tcxo.default_fractional_error() <= hi);
    }

    #[test]
    fn a_valid_fix_disciplines_the_clock() {
        let w = world();
        let rng = RngRegistry::new(1);
        let mut c = OscillatorClock::obu();
        let node = NodeId::new(1);
        for k in 0..10u64 {
            let now = k * NS_PER_S;
            let mut ctx = MobilityCtx::new(now, &w, &rng);
            let believed = c.read(&mut ctx, node, &FixQuality::ThreeD);
            let error_ns = believed.abs_diff(now);
            // Within a few hundred nanoseconds of true time: the 30 ns 95 % bound, with a
            // generous allowance for the tail of the normal draw.
            assert!(error_ns < 500, "error {error_ns} ns at t = {now}");
        }
    }

    #[test]
    fn holdover_drifts_at_the_cited_rate() {
        let w = world();
        let rng = RngRegistry::new(2);
        let mut c = OscillatorClock::obu();
        let node = NodeId::new(1);
        // Start disciplined, then lose the fix for 100 s.
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        c.read(&mut ctx, node, &FixQuality::ThreeD);
        let epsilon = c.fractional_error_of(node).expect("drawn");
        assert!(epsilon.abs() <= 2e-6, "ε = {epsilon}");
        // The offset the disciplined read left behind: the time-transfer error.
        let initial = c.offset_s(node).expect("state");
        assert!(
            initial.abs() < 1e-6,
            "the transfer error is nanoseconds: {initial}"
        );
        let mut ctx = MobilityCtx::new(100 * NS_PER_S, &w, &rng);
        let believed = c.read(&mut ctx, node, &FixQuality::NoFix);
        let drift_s = c.offset_s(node).expect("state");
        // 100 s of holdover adds exactly ε·100 s to whatever the discipline left.
        assert!(
            (drift_s - (initial + epsilon * 100.0)).abs() < 1e-15,
            "{drift_s} against {} + {epsilon}·100",
            initial
        );
        let expected = 100 * i128::from(NS_PER_S) + (drift_s * 1e9).round() as i128;
        assert!(
            (believed as i128 - expected).abs() <= 2,
            "believed {believed} against {expected}"
        );
        // At the full 2 ppm, 100 s of holdover is at most 200 µs.
        assert!(c.worst_case_offset_s(100.0) <= 2e-4 + 1e-15);
        assert!(drift_s.abs() <= c.worst_case_offset_s(100.0) + 1e-15);
    }

    #[test]
    fn an_ocxo_drifts_four_thousand_times_more_slowly() {
        let w = world();
        let rng = RngRegistry::new(3);
        let mut obu = OscillatorClock::obu();
        let mut rsu = OscillatorClock::rsu();
        let node = NodeId::new(1);
        for c in [&mut obu, &mut rsu] {
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            c.read(&mut ctx, node, &FixQuality::ThreeD);
        }
        // A day of holdover.
        let day = 86_400.0;
        assert!((obu.worst_case_offset_s(day) - 2e-6 * day).abs() < 1e-12);
        assert!((rsu.worst_case_offset_s(day) - 0.5e-9 * day).abs() < 1e-18);
        assert!(obu.worst_case_offset_s(day) > 1000.0 * rsu.worst_case_offset_s(day));
        // The Rakon figure: ≤ 1.5 µs of phase error over 24-48 h. 0.5 ppb over 24 h is
        // 43 µs, which is a *frequency* bound and not the disciplined phase bound — the
        // card records both, and this test records that they are different quantities.
        assert!(rsu.worst_case_offset_s(day) > 1.5e-6);
    }

    #[test]
    fn dead_reckoning_does_not_discipline() {
        let w = world();
        let rng = RngRegistry::new(4);
        let mut c = OscillatorClock::obu();
        let node = NodeId::new(1);
        let mut ctx = MobilityCtx::new(0, &w, &rng);
        c.read(&mut ctx, node, &FixQuality::ThreeD);
        let mut ctx = MobilityCtx::new(50 * NS_PER_S, &w, &rng);
        c.read(&mut ctx, node, &FixQuality::DeadReckoning);
        let drifted = c.offset_s(node).expect("state").abs();
        assert!(drifted > 0.0, "a dead-reckoning node's clock free-runs");
    }

    #[test]
    fn each_unit_keeps_its_own_error() {
        let w = world();
        let rng = RngRegistry::new(5);
        let mut c = OscillatorClock::obu();
        let mut errors = Vec::new();
        for id in 0..50u32 {
            let node = NodeId::new(id);
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            c.read(&mut ctx, node, &FixQuality::ThreeD);
            let e = c.fractional_error_of(node).expect("drawn");
            // Read again: the error must not change.
            let mut ctx = MobilityCtx::new(NS_PER_S, &w, &rng);
            c.read(&mut ctx, node, &FixQuality::NoFix);
            assert_eq!(c.fractional_error_of(node), Some(e));
            errors.push(e);
        }
        // They are not all the same, and they are inside the band.
        let spread = errors.iter().fold(0.0f64, |a, b| a.max(b.abs()));
        assert!(spread > 0.0 && spread <= 2e-6);
        assert!(errors.iter().any(|e| *e < 0.0) && errors.iter().any(|e| *e > 0.0));
    }

    #[test]
    fn one_nodes_clock_does_not_depend_on_anothers() {
        let w = world();
        let read_alone = {
            let rng = RngRegistry::new(64);
            let mut c = OscillatorClock::obu();
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            c.read(&mut ctx, NodeId::new(9), &FixQuality::ThreeD);
            c.fractional_error_of(NodeId::new(9))
        };
        let read_after = {
            let rng = RngRegistry::new(64);
            let mut c = OscillatorClock::obu();
            for id in [1u32, 4, 7] {
                let mut ctx = MobilityCtx::new(0, &w, &rng);
                c.read(&mut ctx, NodeId::new(id), &FixQuality::ThreeD);
            }
            let mut ctx = MobilityCtx::new(0, &w, &rng);
            c.read(&mut ctx, NodeId::new(9), &FixQuality::ThreeD);
            c.fractional_error_of(NodeId::new(9))
        };
        assert_eq!(read_alone, read_after);
    }

    #[test]
    fn the_card_validates() {
        OscillatorClock::obu().card().validate().expect("validates");
        OscillatorClock::rsu().card().validate().expect("validates");
    }
}
