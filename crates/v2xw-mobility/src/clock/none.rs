//! `clock/drift/none` — the abstract tier's clock (04-models.md §2.10).
//!
//! Node time equals GNSS time when the fix is valid, and it equals simulated time
//! otherwise: drift-free. §2.10 keeps it as the abstract-tier choice, and it is the baseline
//! every drift figure is measured against.

use v2xw_core::FixQuality;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;

use crate::ctx::MobCtx;
use crate::traits::ClockModel;

/// The model id.
pub const MODEL_ID: &str = "clock/drift/none";

/// The model version.
pub const MODEL_VERSION: &str = "1.0.0";

/// A clock that never drifts.
#[derive(Debug, Clone)]
pub struct DriftFreeClock {
    card: ModelCard,
}

impl Default for DriftFreeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl DriftFreeClock {
    /// The clock.
    pub fn new() -> Self {
        Self { card: card() }
    }
}

impl v2xw_core::model::Model for DriftFreeClock {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl ClockModel for DriftFreeClock {
    fn read(&mut self, ctx: &mut dyn MobCtx, _node: NodeId, _gnss: &FixQuality) -> SimTime {
        ctx.now()
    }
}

/// The model card.
pub fn card() -> ModelCard {
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Clock,
        MODEL_VERSION,
        "The abstract tier's clock: node time equals GNSS time, drift-free. The baseline \
         every drift figure is measured against, and what a scenario that is not studying \
         time wants.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![Equation {
        name: "believed time".to_string(),
        latex_or_text: "believed = true".to_string(),
        notes: None,
    }];
    card.parameters = vec![Parameter::new(
        "drift",
        "1",
        serde_json::json!(0.0),
        Source::new(
            SourceKind::Code,
            "04-models.md §2.10: `clock/drift/none` is drift-free by definition",
        ),
    )];
    card.assumptions =
        vec!["The node's clock is perfect, whatever its fix quality says.".to_string()];
    card.limitations = vec![
        "A scenario that studies replay windows or `generationTime` plausibility must use \
         `clock/drift/tcxo-ocxo`: this model cannot produce a stale or future timestamp at \
         all."
            .to_string(),
    ];
    card.ignores = vec!["Oscillator drift, holdover and time-transfer error.".to_string()];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["clock::none::tests::it_never_drifts".to_string()],
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

    #[test]
    fn it_never_drifts() {
        let w = ring(&RingParams::default()).expect("a ring");
        let rng = RngRegistry::new(1);
        let mut c = DriftFreeClock::new();
        for k in 0..5u64 {
            let now = k * 37 * NS_PER_S;
            let mut ctx = MobilityCtx::new(now, &w, &rng);
            for fix in FixQuality::ALL {
                assert_eq!(c.read(&mut ctx, NodeId::new(1), &fix), now);
            }
        }
    }

    #[test]
    fn the_card_validates_and_is_abstract_tier() {
        let c = DriftFreeClock::new();
        c.card().validate().expect("validates");
        assert_eq!(c.card().tier, vec![Tier::Abstract]);
        assert!(!c.card().determinism.uses_rng);
    }
}
